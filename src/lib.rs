//! The inkline mode protocol, from a mode server's side: what inkline asks a
//! server to colour, indent and complete a command line while it is typed, and
//! how the server answers.
//!
//! A server first writes one line, `inkline-mode 1`, followed by the names of
//! the extra requests it answers, each after a space (`inkline-mode 1 indent
//! complete`); inkline ignores names it does not know. inkline then writes
//! requests to the server's stdin:
//!
//! ```text
//! :request ID
//! :cwd LEN
//! BYTES
//! :arg final|raw LEN
//! BYTES
//! :done
//! ```
//!
//! with one `:arg` per argument, the command's name first. The server answers
//! each on stdout, in order:
//!
//! ```text
//! :span ARG START END KIND
//! :error ARG START END MESSAGE      (or :error - - - MESSAGE)
//! :end ID
//! ```
//!
//! A server that names `indent` on its first line may also be asked where a new
//! line in an argument goes: an `:indent ID` request, with the same `:cwd` and
//! `:arg` blocks and then `:at ARG OFFSET` (where the line breaks) before
//! `:done`. It answers `:depth NEW CURRENT` (how deep the new line goes, and
//! how deep the line being split goes, or `-` to leave it as it is), or
//! nothing, then `:end ID`.
//!
//! A server that names `complete` on its first line may be asked for completion
//! items at a place in an argument: a `:complete ID` request, with the same
//! `:cwd` and `:arg` blocks and then `:at ARG OFFSET` before `:done`, as for
//! `:indent`. It answers zero or more items, each an `:item START END TEXT`
//! line (the bytes `START..END` of the `:at` argument that the item replaces)
//! with an optional `:note TEXT` line right after it, then `:end ID`.
//!
//! Lengths and offsets count bytes. inkline's `docs/mode-protocol.md` describes
//! the whole of the protocol.
//!
//! A server implements [`Mode`] and hands it to [`serve`], which writes the
//! first line and answers each request until the input ends or inkline goes;
//! [`serve_stdio`] does that on stdin and stdout. [`read_request`] and the
//! `write_*` functions are there for a server that runs its own loop.

mod reply;
mod request;

pub use reply::{
    Depths, Item, MAX_ITEM_BYTES, MAX_ITEMS, MAX_SPAN_BYTES, MAX_TEXT_BYTES, Reply, ReplyError,
    Span, SpanKind, items_fit, wanted_first, write_indent, write_items, write_reply,
};
pub use request::{Arg, Ask, At, Request, read_request};

use std::io::{self, BufRead, Write};

/// The README's example, built as a doc test.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// What a mode server answers: colours and errors for every colour request, and
/// indentation and completion items when it names them on its first line.
pub trait Mode {
    /// Whether the server answers `:indent` requests, which its first line then
    /// names.
    const INDENT: bool = false;
    /// Whether the server answers `:complete` requests, which its first line
    /// then names.
    const COMPLETE: bool = false;

    /// The answer to a colour request: the spans to colour, and the error to
    /// underline, if any.
    fn colors(&mut self, request: &Request) -> Reply;

    /// The answer to an `:indent` request, `at` being its `:at` place, where
    /// the line breaks. `None` sends no depths. inkline asks only a server
    /// whose first line names `indent` (see [`Mode::INDENT`]).
    fn indent(&mut self, request: &Request, at: At) -> Option<Depths> {
        let _ = (request, at);
        None
    }

    /// The answer to a `:complete` request, `at` being its `:at` place, where
    /// the cursor is: the items, each replacing some bytes of the `:at`
    /// argument. A reply holds only the first ones that fit (see
    /// [`write_items`]). inkline asks only a server whose first line names
    /// `complete` (see [`Mode::COMPLETE`]).
    fn complete(&mut self, request: &Request, at: At) -> Vec<Item> {
        let _ = (request, at);
        Vec::new()
    }
}

/// The first line a server for `M` writes, without its newline: the protocol's
/// name and version, then the extra requests `M` answers.
pub fn greeting<M: Mode>() -> String {
    let mut line = String::from("inkline-mode 1");
    if M::INDENT {
        line.push_str(" indent");
    }
    if M::COMPLETE {
        line.push_str(" complete");
    }
    line
}

/// Be the mode server: write [`greeting`], then answer each request read from
/// `input` on `output` with `mode`, flushing after each reply. It ends with
/// `Ok` when `input` ends before a request starts, or when inkline goes: a
/// closed output ([`io::ErrorKind::BrokenPipe`]) or a reset connection
/// ([`io::ErrorKind::ConnectionReset`]). Any other error is returned as it
/// came: from [`read_request`] when reading a request, or from `output` when
/// writing or flushing it.
///
/// A reply is written line by line, so `output` is best buffered (such as a
/// [`io::BufWriter`]); the flush after each reply sends it.
pub fn serve<M: Mode>(
    input: &mut impl BufRead,
    output: &mut impl Write,
    mode: &mut M,
) -> io::Result<()> {
    match answer_each(input, output, mode) {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(())
        }
        done => done,
    }
}

/// [`serve`] on this process's stdin and stdout, with stdout buffered.
pub fn serve_stdio<M: Mode>(mode: &mut M) -> io::Result<()> {
    let mut output = io::BufWriter::new(io::stdout().lock());
    serve(&mut io::stdin().lock(), &mut output, mode)
}

/// `items`, the answer to a `:complete` `request` with the cursor `at`, with
/// the items that start with the typed text first when they do not all fit (see
/// [`wanted_first`]). An item's typed text is the bytes of the `:at` argument
/// from the item's start to the cursor. inkline drops an item when the cursor
/// is outside its range or the range goes past the argument's end (see
/// [`Item`]); such an item is not counted.
///
/// An item whose text starts with a quote mark (`` ` ``, `'` or `"`) also
/// counts when the text after the mark starts with the typed text. inkline
/// counts it only when the mark goes on the line as it is, but a request does
/// not say how a `final` argument is quoted at the cursor, so this always
/// counts it.
///
/// inkline ranks first the items that start with the typed text. With
/// `inkline-completion-ignore-case` it ranks next the items that start with it
/// in another case; this does not count those.
pub fn typed_first(items: Vec<Item>, request: &Request, at: At) -> Vec<Item> {
    let arg = request.args.get(at.arg).map_or(&[][..], |a| &a.bytes[..]);
    reply::prefer_typed(items, arg, at.offset)
}

/// [`serve`], where inkline going is an error too.
fn answer_each<M: Mode>(
    input: &mut impl BufRead,
    output: &mut impl Write,
    mode: &mut M,
) -> io::Result<()> {
    writeln!(output, "{}", greeting::<M>())?;
    output.flush()?;
    while let Some(request) = read_request(input)? {
        match request.ask {
            Ask::Colors => write_reply(output, request.id, &mode.colors(&request))?,
            Ask::Indent(at) => write_indent(output, request.id, mode.indent(&request, at))?,
            Ask::Complete(at) => write_items(output, request.id, &mode.complete(&request, at))?,
        }
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Colours the whole of each argument after the command's name as a
    /// `variable`, indents one step past the `:at` argument's index, and offers
    /// that argument's text as its one item.
    struct Echo;

    impl Mode for Echo {
        const INDENT: bool = true;
        const COMPLETE: bool = true;

        fn colors(&mut self, request: &Request) -> Reply {
            let spans = request
                .args
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(_, a)| !a.bytes.is_empty())
                .map(|(arg, a)| Span {
                    arg,
                    at: 0..a.bytes.len(),
                    kind: SpanKind::Variable,
                })
                .collect();
            Reply { spans, error: None }
        }

        fn indent(&mut self, _: &Request, at: At) -> Option<Depths> {
            Some(Depths {
                new: at.arg + 1,
                current: None,
            })
        }

        fn complete(&mut self, request: &Request, at: At) -> Vec<Item> {
            let bytes = &request.args[at.arg].bytes;
            vec![Item {
                at: 0..at.offset,
                text: String::from_utf8_lossy(bytes).into_owned(),
                note: None,
            }]
        }
    }

    /// Answers colour requests only.
    struct Plain;

    impl Mode for Plain {
        fn colors(&mut self, _: &Request) -> Reply {
            Reply::default()
        }
    }

    fn served(mode: &mut impl Mode, input: &str) -> String {
        let mut out = Vec::new();
        serve(&mut input.as_bytes(), &mut out, mode).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_greeting_names_what_the_mode_answers() {
        assert_eq!(greeting::<Echo>(), "inkline-mode 1 indent complete");
        assert_eq!(greeting::<Plain>(), "inkline-mode 1");
        assert_eq!(served(&mut Echo, ""), "inkline-mode 1 indent complete\n");
    }

    #[test]
    fn serve_greets_then_answers_each_request_in_order() {
        let input = ":request 1\n:cwd 1\n/\n:arg final 1\nc\n:arg final 3\nabc\n:done\n\
                     :indent 2\n:cwd 1\n/\n:arg final 1\nc\n:arg final 3\nabc\n:at 1 3\n:done\n\
                     :complete 3\n:cwd 1\n/\n:arg final 1\nc\n:arg final 3\nabc\n:at 1 2\n:done\n";
        assert_eq!(
            served(&mut Echo, input),
            "inkline-mode 1 indent complete\n\
             :span 1 0 3 variable\n:end 1\n\
             :depth 2 -\n:end 2\n\
             :item 0 2 abc\n:end 3\n"
        );
    }

    #[test]
    fn a_mode_without_indent_or_complete_still_ends_each_reply() {
        let input = ":indent 1\n:cwd 0\n\n:arg final 1\nc\n:at 0 1\n:done\n\
                     :complete 2\n:cwd 0\n\n:arg final 1\nc\n:at 0 1\n:done\n";
        assert_eq!(
            served(&mut Plain, input),
            "inkline-mode 1\n:end 1\n:end 2\n"
        );
    }

    /// Fails every read or write with `kind`. A socket inkline has closed fails
    /// a write with `BrokenPipe`, and a read with `ConnectionReset` when a
    /// reply was left unread.
    struct Gone(io::ErrorKind);

    impl io::Read for Gone {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(self.0.into())
        }
    }

    impl Write for Gone {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(self.0.into())
        }
    }

    #[test]
    fn serving_ends_quietly_when_inkline_goes() {
        use io::ErrorKind::{BrokenPipe, ConnectionReset};
        let input = ":request 1\n:cwd 1\n/\n:arg final 1\nc\n:done\n";
        for kind in [BrokenPipe, ConnectionReset] {
            let mut output = Gone(kind);
            assert!(serve(&mut input.as_bytes(), &mut output, &mut Plain).is_ok());
            let mut input = io::BufReader::new(Gone(kind));
            assert!(serve(&mut input, &mut Vec::new(), &mut Plain).is_ok());
        }
        // Any other failure is still an error.
        let mut output = Gone(io::ErrorKind::PermissionDenied);
        let err = serve(&mut input.as_bytes(), &mut output, &mut Plain).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    /// Keeps what was written, and what had been written at each flush.
    #[derive(Default)]
    struct Flushes {
        written: Vec<u8>,
        at_flush: Vec<String>,
    }

    impl Write for Flushes {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.at_flush
                .push(String::from_utf8(self.written.clone()).unwrap());
            Ok(())
        }
    }

    #[test]
    fn serve_flushes_after_the_greeting_and_each_reply() {
        let input = ":request 1\n:cwd 0\n\n:arg final 1\nc\n:done\n\
                     :request 2\n:cwd 0\n\n:arg final 1\nc\n:done\n";
        let mut output = Flushes::default();
        serve(&mut input.as_bytes(), &mut output, &mut Plain).unwrap();
        assert_eq!(
            output.at_flush,
            [
                "inkline-mode 1\n",
                "inkline-mode 1\n:end 1\n",
                "inkline-mode 1\n:end 1\n:end 2\n"
            ]
        );
    }

    #[test]
    fn a_request_that_breaks_the_protocol_stops_serving() {
        let mut out = Vec::new();
        let err = serve(&mut &b"hello\n"[..], &mut out, &mut Plain).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(out, b"inkline-mode 1\n");
    }
}
