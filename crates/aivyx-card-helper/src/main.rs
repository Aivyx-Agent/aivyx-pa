//! `aivyx-pa-card`: one JSON request on stdin, one reply on stdout (A18).
use std::io::Read;

use aivyx_card_helper::{ErrorKind, Reply, Request, serve};

fn main() {
    let mut input = zeroize::Zeroizing::new(Vec::new());
    let reply = match std::io::stdin().take(64 * 1024).read_to_end(&mut input) {
        Err(e) => Reply::Error {
            kind: ErrorKind::Other,
            message: format!("read stdin: {e}"),
            tries_left: None,
        },
        Ok(_) => match serde_json::from_slice::<Request>(&input) {
            Err(e) => Reply::Error {
                kind: ErrorKind::Other,
                message: format!("bad request: {e}"),
                tries_left: None,
            },
            Ok(req) => serve::handle(
                req,
                &|serial| match serial {
                    Some(s) => aivyx_yubi::discovery::discover_real_card_by_serial(s),
                    None => aivyx_yubi::discovery::discover_real_card(),
                },
                &aivyx_yubi::discovery::list_cards,
            ),
        },
    };
    // The reply may carry the shared secret: zeroize the text too.
    let out = zeroize::Zeroizing::new(serde_json::to_string(&reply).expect("reply serializes"));
    println!("{}", out.as_str());
}
