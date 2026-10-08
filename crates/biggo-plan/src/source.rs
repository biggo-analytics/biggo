//! Where a call such as `read_sql` reads from: a file, or a database server named by a URL.

use crate::plan::Format;

/// What a source is, when it is the address of a database server and not the path of a
/// file: its format, and the address as it is safe to show, without the password.
pub fn server(source: &str) -> Option<(Format, String)> {
    let (scheme, rest) = source.split_once("://")?;
    let format = match scheme.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" => Format::Postgres,
        _ => return None,
    };
    Some((format, format!("{scheme}://{}", hidden(rest))))
}

/// An address after its scheme, with its password replaced by `***`: the part between the
/// user and the `@` of the host, and a `password` among the settings that follow a `?`.
fn hidden(address: &str) -> String {
    let (place, settings) = match address.split_once('?') {
        Some((place, settings)) => (place, Some(settings)),
        None => (address, None),
    };
    // The host starts after the last `@` before the first `/`.
    let host = place.find('/').unwrap_or(place.len());
    let mut shown = match place[..host].rsplit_once('@') {
        Some((login, server)) => match login.split_once(':') {
            Some((user, _)) => format!("{user}:***@{server}{}", &place[host..]),
            None => format!("{login}@{server}{}", &place[host..]),
        },
        None => place.to_string(),
    };
    if let Some(settings) = settings {
        let settings = settings
            .split('&')
            .map(|setting| match setting.split_once('=') {
                Some((name, _)) if name.eq_ignore_ascii_case("password") => format!("{name}=***"),
                _ => setting.to_string(),
            });
        shown.push('?');
        shown.push_str(&settings.collect::<Vec<_>>().join("&"));
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_told_from_a_file() {
        assert_eq!(server("data/shop.db"), None);
        assert_eq!(server("C://data/shop.db"), None);
        assert_eq!(server("https://example.com/x.csv"), None);
        let (format, shown) = server("postgres://db.example.com/shop").unwrap();
        assert_eq!(
            (format, shown.as_str()),
            (Format::Postgres, "postgres://db.example.com/shop")
        );
        assert_eq!(server("PostgreSQL://h/d").unwrap().0, Format::Postgres);
    }

    #[test]
    fn a_password_is_never_shown() {
        let shown = |address| server(address).unwrap().1;
        assert_eq!(
            shown("postgres://app:s3cret@db:5432/shop"),
            "postgres://app:***@db:5432/shop"
        );
        assert_eq!(shown("postgres://app@db/shop"), "postgres://app@db/shop");
        // A password may hold the characters that otherwise end a part.
        assert_eq!(
            shown("postgres://app:p@ss:w0rd@db/shop"),
            "postgres://app:***@db/shop"
        );
        assert_eq!(
            shown("postgres://app:pw@db/shop?sslmode=require"),
            "postgres://app:***@db/shop?sslmode=require"
        );
        assert_eq!(
            shown("postgres://db/shop?user=app&password=s3cret&sslmode=disable"),
            "postgres://db/shop?user=app&password=***&sslmode=disable"
        );
        assert_eq!(shown("postgres://app:pw@db"), "postgres://app:***@db");
    }
}
