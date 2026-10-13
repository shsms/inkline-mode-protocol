# inkline-mode-protocol

The [inkline](https://github.com/shsms/inkline) mode protocol, from a mode
server's side: reading the requests inkline sends while a command line is typed,
and writing the replies that colour it, underline its first error, indent a new
line and offer completion items.

A server implements `Mode` and runs `serve_stdio`, which serves its stdin and
stdout (`examples/words.rs`):

```rust,no_run
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
            .map(|(arg, a)| Span { arg, at: 0..a.bytes.len(), kind: SpanKind::String })
            .collect();
        Reply { spans, error: None }
    }
}

fn main() -> std::io::Result<()> {
    serve_stdio(&mut Words)
}
```

A mode that answers `:indent` or `:complete` requests sets `Mode::INDENT` or
`Mode::COMPLETE` and implements `Mode::indent` or `Mode::complete`; the first
line the server writes then names them.

inkline turns a server off when a reply passes 1 MiB, and keeps at most 1,000
items. To stay within that, spans stop before 900 KiB of lines, items stop at
1,000 or at 900 KiB, and an error's message and a note are each cut to one line
of at most 4 KiB. An item whose text is empty, over 4 KiB, or holds a control
character other than a tab is left out. An item's text is never cut or changed:
it is sent as the server gave it, or not at all.

inkline's `docs/mode-protocol.md` describes the whole of the protocol.

## License

GPL-3.0-only.
