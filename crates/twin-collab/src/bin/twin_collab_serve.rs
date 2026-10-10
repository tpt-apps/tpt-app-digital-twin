//! Serve one twin room over loopback TCP, for multi-process tests and demos.
//!
//! Usage: `twin_collab_serve <bind-addr> <owner-token> <editor-token> <viewer-token>`
//!
//! Prints `listening <addr>` on stdout once bound, then runs until killed. The
//! room is `twin`. Tokens are plain strings given on the command line, so this
//! is for local testing only, not a deployment.

use std::io::Write;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use tpt_vertex_collab::protocol::AccessLevel;
use tpt_vertex_collab::server::MemoryAuth;
use twin_collab::hub::TwinHub;
use twin_collab::transport::serve;

const ROOM: &str = "twin";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [bind, owner, editor, viewer] = match args.as_slice() {
        [a, b, c, d] => [a, b, c, d],
        _ => {
            eprintln!("usage: twin_collab_serve <bind-addr> <owner> <editor> <viewer>");
            return ExitCode::from(2);
        }
    };

    let mut auth = MemoryAuth::new();
    for (token, subject, level) in [
        (owner, "owner", AccessLevel::Owner),
        (editor, "editor", AccessLevel::Editor),
        (viewer, "viewer", AccessLevel::Viewer),
    ] {
        auth.add_token(token, subject);
        auth.grant(ROOM, subject, level);
    }

    match serve(TwinHub::new(auth), bind, false) {
        Ok(addr) => {
            println!("listening {addr}");
            let _ = std::io::stdout().flush();
        }
        Err(e) => {
            eprintln!("cannot serve on {bind}: {e}");
            return ExitCode::FAILURE;
        }
    }
    // The server runs on background threads. Keep the process alive until killed.
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
