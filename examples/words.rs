//! The smallest mode server: it colours every argument after the command's name
//! as a string. Build it with `cargo build --example words`, then point
//! `inkline-define-mode` at `target/debug/examples/words`.

use inkline_mode_protocol::{Mode, Reply, Request, Span, SpanKind, serve_stdio};

struct Words;

impl Mode for Words {
    fn colors(&mut self, request: &Request) -> Reply {
        // Colour every argument after the command's name as a string.
        let spans = request
            .args
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, arg)| !arg.bytes.is_empty())
            .map(|(arg, a)| Span {
                arg,
                at: 0..a.bytes.len(),
                kind: SpanKind::String,
            })
            .collect();
        Reply { spans, error: None }
    }
}

fn main() -> std::io::Result<()> {
    serve_stdio(&mut Words)
}
