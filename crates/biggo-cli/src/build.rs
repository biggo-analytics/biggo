//! Standalone executables. `biggo build` copies the `biggo` executable itself and stores the
//! program in a block of bytes reserved inside it; an executable that finds a program there
//! runs it instead of reading a command line.

use std::path::{Component, Path, PathBuf};

use biggo_eval::{Session, normalize};
use biggo_syntax::shown_path;

/// The size of the reserved block, which bounds the size of an embedded program.
const CAPACITY: usize = 256 << 10;

/// Marks the start of the block in the executable file.
const MAGIC: [u8; 16] = *b"\x00biggo-payload\x01\x00";

/// The marker, the length of what follows as eight little-endian bytes, then the files of the
/// program: the one to run, then the files it imports. Each is its name and its source, both
/// preceded by their length as four little-endian bytes. An imported file is named by its
/// path from the directory of the program. A length of zero means there is no program.
#[used]
static PAYLOAD: [u8; CAPACITY] = {
    let mut block = [0; CAPACITY];
    let mut index = 0;
    while index < MAGIC.len() {
        block[index] = MAGIC[index];
        index += 1;
    }
    block
};

const HEADER: usize = MAGIC.len() + 8;

/// The block as it is in this executable's file, which `biggo build` may have filled in. The
/// compiler only knows the empty block above, so it must not see through this read.
fn payload() -> &'static [u8] {
    std::hint::black_box(&PAYLOAD[..])
}

/// The program embedded in this executable, if there is one: the name and the source of the
/// file to run, then of the files it imports.
pub fn embedded() -> Option<Vec<(&'static str, &'static str)>> {
    let payload = payload();
    let length = u64::from_le_bytes(payload[MAGIC.len()..HEADER].try_into().ok()?) as usize;
    if length == 0 {
        return None;
    }
    let mut rest = payload.get(HEADER..HEADER + length)?;
    let mut text = || {
        let (length, after) = rest.split_first_chunk::<4>()?;
        let (text, after) = after.split_at_checked(u32::from_le_bytes(*length) as usize)?;
        rest = after;
        std::str::from_utf8(text).ok()
    };
    let mut files = Vec::new();
    while let Some(name) = text() {
        files.push((name, text()?));
    }
    (!files.is_empty()).then_some(files)
}

/// The path that leads from the directory `base` to `path`; both are normalized.
fn path_from(base: &Path, path: &Path) -> PathBuf {
    let mut base = base.components().peekable();
    let mut path = path.components().peekable();
    while base.peek().is_some() && base.peek() == path.peek() {
        base.next();
        path.next();
    }
    let up = base.map(|_| Component::ParentDir);
    up.chain(path).collect()
}

/// Writes an executable to `output` that runs the program at `path`.
pub fn build(path: &str, output: &str) -> Result<(), String> {
    let shown = shown_path(path);
    let source =
        std::fs::read_to_string(path).map_err(|err| format!("cannot read {shown}: {err}"))?;
    // An executable that cannot run is of no use: check the program first.
    let dir = normalize(Path::new(path).parent().unwrap_or(Path::new("")));
    let mut session = Session::new(std::io::sink());
    session.vm().set_base_dir(&dir);
    if let Err(errors) = session.check(&shown, &source) {
        let count = errors.diagnostics.len();
        let plural = if count == 1 { "" } else { "s" };
        return Err(format!(
            "{}\n{count} error{plural} in {}",
            errors.render(),
            errors.name
        ));
    }
    let name = Path::new(path)
        .file_name()
        .map_or(path.into(), |name| name.to_string_lossy());
    // The files the program imports go into the executable with it.
    let mut files = vec![(name.to_string(), source)];
    let mut imported: Vec<&Path> = session.imported().collect();
    imported.sort();
    for import in imported {
        let source = std::fs::read_to_string(import)
            .map_err(|err| format!("cannot read {}: {err}", shown_path(import)))?;
        files.push((
            path_from(&dir, import).to_string_lossy().into_owned(),
            source,
        ));
    }
    let mut program = Vec::new();
    for (name, source) in &files {
        for text in [name, source] {
            program.extend((text.len() as u32).to_le_bytes());
            program.extend(text.as_bytes());
        }
    }
    if program.len() > CAPACITY - HEADER {
        let limit = (CAPACITY - HEADER) >> 10;
        return Err(format!(
            "{shown} is too large to embed; the limit is {limit} KiB"
        ));
    }

    let tool = std::env::current_exe()
        .map_err(|err| format!("cannot find the biggo executable: {err}"))?;
    let mut executable =
        std::fs::read(&tool).map_err(|err| format!("cannot read {}: {err}", shown_path(&tool)))?;
    let marker = &payload()[..MAGIC.len()];
    let mut places = executable
        .windows(MAGIC.len())
        .enumerate()
        .filter(|(_, bytes)| *bytes == marker);
    let (Some((start, _)), None) = (places.next(), places.next()) else {
        return Err("this biggo executable has no room for a program; reinstall biggo".into());
    };
    let length = (program.len() as u64).to_le_bytes();
    executable[start + MAGIC.len()..start + HEADER].copy_from_slice(&length);
    executable[start + HEADER..start + HEADER + program.len()].copy_from_slice(&program);

    std::fs::write(output, executable)
        .map_err(|err| format!("cannot write {}: {err}", shown_path(output)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(output, executable)
            .map_err(|err| format!("cannot make {output} executable: {err}"))?;
    }
    // macOS refuses to run an executable whose contents no longer match its signature.
    #[cfg(target_os = "macos")]
    {
        let signed = std::process::Command::new("codesign")
            .args(["--force", "--sign", "-", output])
            .output();
        match signed {
            Ok(result) if result.status.success() => {}
            Ok(result) => {
                let reason = String::from_utf8_lossy(&result.stderr);
                return Err(format!("cannot sign {output}: {}", reason.trim()));
            }
            Err(err) => return Err(format!("cannot run codesign to sign {output}: {err}")),
        }
    }
    Ok(())
}
