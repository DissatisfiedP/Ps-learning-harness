//! `cargo run -p harness --bin plugin -- test <dir>` (build prompt W5).
//!
//! Validates the manifest, its files, the schemas and fragments, runs a Luau
//! type check when `luau-analyze` is installed, then runs every test file in
//! the sandboxed runtime.

use std::path::Path;
use std::process::ExitCode;

use harness::plugins::manifest;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    let dir = args.next();
    let Some(dir) = dir else {
        eprintln!("usage: plugin test <dir> | plugin validate <dir>");
        return ExitCode::from(2);
    };
    let path = Path::new(&dir);
    let result = match command.as_deref() {
        Some("test") => test(path),
        Some("validate") => validate(path).map(|m| format!("ok: {} {}", m.id, m.version)),
        _ => {
            eprintln!("usage: plugin test <dir> | plugin validate <dir>");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn validate(path: &Path) -> Result<manifest::PluginManifest, String> {
    let manifest = manifest::read(path)?;
    // The manifest validator already checked files, limits and agents; check
    // every schema parses too.
    for tool in &manifest.tools {
        let text = std::fs::read_to_string(path.join(&tool.input))
            .map_err(|e| format!("{}: {e}", tool.input))?;
        let schema: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", tool.input))?;
        if !schema.is_object() {
            return Err(format!("{} must be a JSON object", tool.input));
        }
    }
    Ok(manifest)
}

fn test(path: &Path) -> Result<String, String> {
    let manifest = validate(path)?;
    let mut out = format!(
        "manifest ok: {} {} ({} tools)\n",
        manifest.id,
        manifest.version,
        manifest.tools.len()
    );
    match harness::plugins::analyze_package(path) {
        Some(Ok(_)) => out.push_str("luau-analyze: ok\n"),
        Some(Err(e)) => return Err(format!("luau-analyze failed:\n{e}")),
        None => out.push_str(
            "luau-analyze: not installed (install the Luau CLI to type-check; tests still run)\n",
        ),
    }
    let ran = harness::plugins::test_package(path)?;
    out.push_str(&format!("{} test(s) passed:\n", ran.len()));
    for name in ran {
        out.push_str(&format!("  {name}\n"));
    }
    Ok(out)
}
