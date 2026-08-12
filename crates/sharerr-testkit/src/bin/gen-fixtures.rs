//! Write the synthetic media library to a directory.
//!
//! The compose stack mounts the result as its library, so the live stack and the
//! unit tests share one definition of what the fixtures are. Regenerating is
//! idempotent — the content is seeded, so the same bytes come back every time.
//!
//! ```text
//! cargo run -p sharerr-testkit --bin gen-fixtures -- tests/fixtures/media
//! ```

#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: gen-fixtures <directory>");
        eprintln!("       writes a synthetic tv/ and movies/ tree with invented titles");
        return ExitCode::FAILURE;
    };

    let tv = match sharerr_testkit::tv_library(&root) {
        Ok(library) => library,
        Err(err) => {
            eprintln!("could not write the tv library: {err}");
            return ExitCode::FAILURE;
        }
    };

    let movies = match sharerr_testkit::movie_library(&root) {
        Ok(library) => library,
        Err(err) => {
            eprintln!("could not write the movie library: {err}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "wrote {} synthetic file(s) under {}",
        tv.files.len() + movies.files.len(),
        root.display()
    );
    for file in tv.files.iter().chain(&movies.files) {
        println!("  {} ({} bytes)", file.disk_path.display(), file.size);
    }
    println!();
    println!("every title here is invented; nothing corresponds to real content");

    ExitCode::SUCCESS
}
