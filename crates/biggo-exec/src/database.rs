//! Reads from and writes to a PostgreSQL server. Rows cross the connection as CSV, with
//! `COPY`: it is the fastest way in and out of the server, it does not hold a result in
//! memory twice, and what arrives is text, which is read as the text of a file is.

use std::io::{self, BufRead, Read};
use std::sync::Arc;

use arrow::csv::{ReaderBuilder, WriterBuilder};
use arrow::datatypes::{DataType as ArrowType, Field as ArrowField, Schema as ArrowSchema};
use biggo_plan::{DataType, Plan, Scan};
use postgres::{Client, Config};
use regex::Regex;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::scan::{BATCH_ROWS, Piece, Shape};
use crate::{Error, Result, collect, for_text_file, sql_name};

/// How a null is written in the CSV that crosses the connection, so that it is not taken
/// for an empty string: a text that no value is likely to be, between two characters that
/// are not written by hand.
const NULL: &str = "\u{1f}null\u{1f}";

/// The rows that a `COPY` sends. The connection must not be asked for more of them once
/// they have ended, which a reader of CSV does to make sure of the end.
struct Sent<R> {
    rows: R,
    ended: bool,
}

impl<R: BufRead> Read for Sent<R> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        let ready = self.fill_buf()?;
        let taken = ready.len().min(into.len());
        into[..taken].copy_from_slice(&ready[..taken]);
        self.consume(taken);
        Ok(taken)
    }
}

impl<R: BufRead> BufRead for Sent<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.ended {
            return Ok(&[]);
        }
        let ready = self.rows.fill_buf()?;
        self.ended = ready.is_empty();
        Ok(ready)
    }

    fn consume(&mut self, amount: usize) {
        self.rows.consume(amount);
    }
}

/// What went wrong with a server: what it said, without the words of the driver around it,
/// or what the driver found, with the cause that it keeps underneath.
fn reason(err: &postgres::Error) -> String {
    if let Some(said) = err.as_db_error() {
        return said.message().to_string();
    }
    let mut reason = err.to_string();
    let mut cause = std::error::Error::source(err);
    while let Some(under) = cause {
        reason.push_str(&format!(": {under}"));
        cause = under.source();
    }
    reason
}

/// How a connection is protected, which the `sslmode` of an address says in the words that
/// PostgreSQL's own tools use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protection {
    /// `disable`: nothing is encrypted.
    Open,
    /// `prefer`, which is what an address means when it does not say, and `require`: the
    /// connection is encrypted, and the server is not asked to prove who it is.
    Encrypted,
    /// `verify-ca` and `verify-full`: encrypted, and the certificate of the server must be
    /// one that this machine trusts, made out for the name in the address.
    Verified,
}

/// The protection that an address asks for, and the address as the driver takes it: the
/// driver does not know the two modes that check the server.
fn protection(address: &str) -> (Protection, String) {
    let Some((place, settings)) = address.split_once('?') else {
        return (Protection::Encrypted, address.to_string());
    };
    let mut protection = Protection::Encrypted;
    let settings = settings.split('&').map(|setting| match setting {
        "sslmode=disable" => {
            protection = Protection::Open;
            setting
        }
        "sslmode=verify-ca" | "sslmode=verify-full" => {
            protection = Protection::Verified;
            "sslmode=require"
        }
        _ => setting,
    });
    let settings: Vec<&str> = settings.collect();
    (protection, format!("{place}?{}", settings.join("&")))
}

/// Takes the certificate of any server: the connection is encrypted, but not known to
/// reach the server it was meant for.
#[derive(Debug)]
struct AnyServer(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyServer {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        let known = &self.0.signature_verification_algorithms;
        rustls::crypto::verify_tls12_signature(message, certificate, signature, known)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        let known = &self.0.signature_verification_algorithms;
        rustls::crypto::verify_tls13_signature(message, certificate, signature, known)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// How the connection is encrypted. With `verified`, the server is checked against the
/// certificates that the system trusts, or, on a system that has none, the public ones.
fn encryption(verified: bool) -> Result<MakeRustlsConnect> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|err| Error(format!("cannot set up an encrypted connection: {err}")))?;
    let config = match verified {
        true => {
            let mut trusted = rustls::RootCertStore::empty();
            for certificate in rustls_native_certs::load_native_certs().certs {
                // A certificate that cannot be read is one the system should not have had.
                let _ = trusted.add(certificate);
            }
            if trusted.is_empty() {
                trusted.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }
            config.with_root_certificates(trusted)
        }
        false => config
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyServer(provider))),
    };
    Ok(MakeRustlsConnect::new(config.with_no_client_auth()))
}

/// Connects to the server at `address`, which messages call `shown`: the address without
/// its password.
fn connect(address: &str, shown: &str) -> Result<Client> {
    let (protection, address) = protection(address);
    // What is wrong with an address is not repeated from the driver, whose words might
    // hold a part of the password.
    let config: Config = address.parse().map_err(|_| {
        Error(format!(
            "{shown} is not the address of a PostgreSQL server, which is written \
             postgres://user:password@host:port/database"
        ))
    })?;
    let fail = |err: postgres::Error| Error(format!("cannot connect to {shown}: {}", reason(&err)));
    match protection {
        Protection::Open => config.connect(postgres::NoTls).map_err(fail),
        other => {
            let verified = other == Protection::Verified;
            config.connect(encryption(verified)?).map_err(fail)
        }
    }
}

/// The name of a table as it is written in a statement, with the schema it is in when the
/// name has one: `sales.daily` is the table `daily` of the schema `sales`.
fn table_name(name: &str) -> String {
    let parts: Vec<String> = name.split('.').map(sql_name).collect();
    parts.join(".")
}

pub(crate) fn pieces(scan: &Scan, shape: &Arc<Shape>) -> Result<Vec<Piece>> {
    let Some(query) = scan.query.clone() else {
        return Err(Error(
            "internal error: a database scan without a query".into(),
        ));
    };
    let address = scan.path.to_string_lossy().into_owned();
    // The server can stop early only if no row is dropped here after it.
    let limit = scan.limit.filter(|_| scan.filters.is_empty());
    let shape = shape.clone();
    let piece = move || {
        let shown = &shape.file;
        let fail = |err: postgres::Error| Error(format!("{shown}: {}", reason(&err)));
        let mut client = connect(&address, shown)?;
        // Preparing the query tells the names of its columns without running it.
        let query = query.trim().trim_end_matches(';');
        let prepared = client.prepare(query).map_err(fail)?;
        let available: Vec<String> = prepared
            .columns()
            .iter()
            .map(|column| column.name().to_string())
            .collect();
        for field in &shape.read.fields {
            if !available.iter().any(|name| *name == *field.name) {
                return Err(shape.missing_column(&field.name, &available));
            }
        }
        // The server sends only the columns that are read, and no more rows than wanted.
        // The query stands on lines of its own, so a comment at its end ends there.
        let columns: Vec<String> = shape
            .read
            .fields
            .iter()
            .map(|f| sql_name(&f.name))
            .collect();
        let mut select = format!(
            "SELECT {} FROM (\n{query}\n) AS biggo_source",
            columns.join(", ")
        );
        if let Some(limit) = limit {
            select.push_str(&format!(" LIMIT {limit}"));
        }
        let copy = format!("COPY ({select}) TO STDOUT (FORMAT CSV, NULL '{NULL}')");
        let rows = Sent {
            rows: client.copy_out(&copy).map_err(fail)?,
            ended: false,
        };

        // Every column arrives as text, which `Shape::finish` reads as the declared type.
        let texts = shape
            .read
            .fields
            .iter()
            .map(|field| ArrowField::new(&*field.name, ArrowType::Utf8, true));
        let texts = Arc::new(ArrowSchema::new(texts.collect::<Vec<_>>()));
        let null = Regex::new(&format!("^{}$", regex::escape(NULL))).expect("the pattern is valid");
        let reader = ReaderBuilder::new(texts)
            .with_header(false)
            .with_null_regex(null)
            .with_batch_size(BATCH_ROWS)
            .build_buffered(rows)?;
        let mut batches = Vec::new();
        for batch in reader {
            let batch = batch.map_err(|err| Error(format!("{shown}: {err}")))?;
            batches.push(shape.finish(&batch)?);
        }
        Ok(batches)
    };
    Ok(vec![Box::new(piece)])
}

/// Writes the rows of `plan` as a table of the database at `address`, in place of the table
/// of that name if there is one. The table changes all at once, when every row is in it, or
/// not at all. A decimal is stored as `numeric`, and a duration as its seconds.
pub fn write_postgres(plan: &Arc<Plan>, address: &str, table: &str) -> Result<()> {
    let shown = match biggo_plan::source::server(address) {
        Some((_, shown)) => shown,
        None => address.to_string(),
    };
    let fail = |err: postgres::Error| Error(format!("{shown}: {}", reason(&err)));
    let schema = plan.schema();
    let columns = schema.fields.iter().map(|field| {
        let kind = match field.ty.dtype {
            DataType::Int => "BIGINT",
            DataType::Float | DataType::Duration => "DOUBLE PRECISION",
            DataType::Bool => "BOOLEAN",
            DataType::Str => "TEXT",
            DataType::Date => "DATE",
            DataType::DateTime => "TIMESTAMP",
            DataType::Decimal => "NUMERIC(38, 6)",
        };
        format!("{} {kind}", sql_name(&field.name))
    });
    let columns: Vec<String> = columns.collect();
    let name = table_name(table);
    // The query may read the very table it replaces, so it runs to its end first.
    let batches = collect(plan)?;

    let mut client = connect(address, &shown)?;
    let mut change = client.transaction().map_err(fail)?;
    let create = format!(
        "DROP TABLE IF EXISTS {name}; CREATE TABLE {name} ({})",
        columns.join(", ")
    );
    change.batch_execute(&create).map_err(fail)?;
    let copy = format!("COPY {name} FROM STDIN (FORMAT CSV, NULL '{NULL}')");
    let mut rows = change.copy_in(&copy).map_err(fail)?;
    {
        let mut writer = WriterBuilder::new()
            .with_header(false)
            .with_null(NULL.to_string())
            .build(&mut rows);
        for batch in &batches {
            writer.write(&for_text_file(batch)?)?;
        }
    }
    rows.finish().map_err(fail)?;
    change.commit().map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_says_how_it_is_protected() {
        let plain = "postgres://app:pw@db/shop";
        assert_eq!(
            protection(plain),
            (Protection::Encrypted, plain.to_string())
        );
        let open = "postgres://db/shop?sslmode=disable&application_name=x";
        assert_eq!(protection(open), (Protection::Open, open.to_string()));
        let required = "postgres://db/shop?sslmode=require";
        assert_eq!(
            protection(required),
            (Protection::Encrypted, required.to_string())
        );
        for mode in ["verify-ca", "verify-full"] {
            let address = format!("postgres://db/shop?connect_timeout=5&sslmode={mode}");
            let taken = "postgres://db/shop?connect_timeout=5&sslmode=require";
            assert_eq!(
                protection(&address),
                (Protection::Verified, taken.to_string())
            );
        }
    }

    #[test]
    fn a_table_can_be_in_a_schema() {
        assert_eq!(table_name("daily"), "\"daily\"");
        assert_eq!(table_name("sales.daily"), "\"sales\".\"daily\"");
        assert_eq!(table_name("odd\"name"), "\"odd\"\"name\"");
    }
}
