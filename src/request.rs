//! Reading a request.

use std::io::{self, BufRead, Read};
use std::ops::Range;
use std::path::Path;

/// The longest `:` line a request may have, newline included.
const MAX_LINE: u64 = 256;

/// The most bytes one `:cwd` or `:arg` may hold. A command line is far smaller;
/// the limit keeps a corrupt length from asking for any amount of memory.
const MAX_BYTES: usize = 16 << 20;

/// One request: the shell's directory and the command's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    pub cwd: Vec<u8>,
    /// Argument 0 is the command's name.
    pub args: Vec<Arg>,
    /// What kind of request this is, and its `:at` place when it has one.
    pub ask: Ask,
}

/// What one request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// A colour request: the spans and the first error.
    Colors,
    /// An `:indent` request, with its `:at` place: an argument's index and a
    /// byte offset in it, at most its length.
    Indent((usize, usize)),
    /// A `:complete` request, with its `:at` place, the same shape as
    /// `Indent`'s.
    Complete((usize, usize)),
}

/// One argument of the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arg {
    /// Bash will still expand it, so `bytes` is the text as typed, not what the
    /// command will get.
    pub raw: bool,
    pub bytes: Vec<u8>,
}

impl Request {
    /// The arguments after the command's name as text; `Err` with the place of
    /// the first bytes that are not UTF-8: the argument, and the bytes of it.
    pub fn words(&self) -> Result<Vec<String>, (usize, Range<usize>)> {
        let mut words = Vec::with_capacity(self.args.len());
        // Argument 0 is the command's name, which may be any of the names the
        // mode is used for.
        for (i, arg) in self.args.iter().enumerate().skip(1) {
            match std::str::from_utf8(&arg.bytes) {
                Ok(text) => words.push(text.to_string()),
                Err(e) => {
                    let start = e.valid_up_to();
                    let end = e.error_len().map_or(arg.bytes.len(), |n| start + n);
                    return Err((i, start..end));
                }
            }
        }
        Ok(words)
    }

    /// The shell's directory, from `:cwd`. `None` when that is not an absolute
    /// UTF-8 path: bash sends an empty one when `PWD` is unset.
    pub fn shell_dir(&self) -> Option<&Path> {
        let dir = Path::new(std::str::from_utf8(&self.cwd).ok()?);
        dir.is_absolute().then_some(dir)
    }
}

/// Read the next request, a colour (`:request`), an `:indent` or a `:complete`
/// one; `None` when the input ends before one starts. Input that is not a
/// request is an [`io::ErrorKind::InvalidData`] error: after it, the stream
/// cannot be followed. So is an `:indent` or `:complete` request without `:at`
/// just before its `:done`, an `:at` in a colour request, and an `:at` outside
/// the request's arguments. Input that ends inside a request is an
/// [`io::ErrorKind::UnexpectedEof`] error, unless a complete line or block
/// before the end is already an [`io::ErrorKind::InvalidData`] error. A `:`
/// line of more than 255 bytes before its newline is an
/// [`io::ErrorKind::InvalidData`] error, even when the input ends inside it.
pub fn read_request(input: &mut impl BufRead) -> io::Result<Option<Request>> {
    let Some(line) = read_line(input)? else {
        return Ok(None);
    };
    // When this request needs an `:at` before its `:done`, the word its kind is
    // named by and the `Ask` it makes of the place; `None` for a colour
    // request, which does not.
    type MakeAsk = fn((usize, usize)) -> Ask;
    let (id, needs_at): (u64, Option<(&str, MakeAsk)>) =
        if let Some(id) = line.strip_prefix(":indent ") {
            (number(id)?, Some(("indent", Ask::Indent)))
        } else if let Some(id) = line.strip_prefix(":complete ") {
            (number(id)?, Some(("complete", Ask::Complete)))
        } else {
            (number(field(&line, ":request ")?)?, None)
        };
    let line = need_line(input)?;
    let cwd = read_bytes(input, number(field(&line, ":cwd ")?)?)?;
    let mut args = Vec::new();
    let ask = loop {
        let line = need_line(input)?;
        if line == ":done" {
            if let Some((name, _)) = needs_at {
                return Err(invalid(format!(
                    "an :{name} request needs :at before :done"
                )));
            }
            break Ask::Colors;
        }
        if let Some((_, ask)) = needs_at
            && let Some(at) = line.strip_prefix(":at ")
        {
            let at = read_at(at, &args)?;
            let done = need_line(input)?;
            if done != ":done" {
                return Err(bad(&done));
            }
            break ask(at);
        }
        let (kind, len) = field(&line, ":arg ")?
            .split_once(' ')
            .ok_or_else(|| bad(&line))?;
        let raw = match kind {
            "final" => false,
            "raw" => true,
            _ => return Err(bad(&line)),
        };
        args.push(Arg {
            raw,
            bytes: read_bytes(input, number(len)?)?,
        });
    };
    Ok(Some(Request { id, cwd, args, ask }))
}

/// The fields of an `:at` line, `ARG OFFSET`: an argument of `args` and a byte
/// offset in it, at most its length.
fn read_at(fields: &str, args: &[Arg]) -> io::Result<(usize, usize)> {
    let (arg, offset) = fields.split_once(' ').ok_or_else(|| bad(fields))?;
    let (arg, offset): (usize, usize) = (number(arg)?, number(offset)?);
    match args.get(arg) {
        Some(a) if offset <= a.bytes.len() => Ok((arg, offset)),
        _ => Err(bad(&format!(":at {fields}"))),
    }
}

/// The rest of `line` after `keyword`, which it must start with.
fn field<'l>(line: &'l str, keyword: &str) -> io::Result<&'l str> {
    line.strip_prefix(keyword).ok_or_else(|| bad(line))
}

/// A decimal number: ASCII digits only.
fn number<T: std::str::FromStr>(text: &str) -> io::Result<T> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad(text));
    }
    text.parse().map_err(|_| bad(text))
}

/// One `:` line, without its newline; `None` at the end of the input. If the
/// input ends inside a line, before its newline, that is an [`ended`] error,
/// unless the line already holds [`MAX_LINE`] bytes and so cannot be a `:`
/// line.
fn read_line(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    input.by_ref().take(MAX_LINE).read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(None);
    }
    if line.pop_if(|b| *b == b'\n').is_none() {
        if (line.len() as u64) < MAX_LINE {
            return Err(ended());
        }
        return Err(bad(&String::from_utf8_lossy(&line)));
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|e| bad(&String::from_utf8_lossy(e.as_bytes())))
}

/// [`read_line`], where the end of the input is an error.
fn need_line(input: &mut impl BufRead) -> io::Result<String> {
    read_line(input)?.ok_or_else(ended)
}

/// `len` bytes, then the newline after them.
fn read_bytes(input: &mut impl BufRead, len: usize) -> io::Result<Vec<u8>> {
    if len > MAX_BYTES {
        return Err(invalid(format!(
            "a :cwd or :arg of {len} bytes is over the limit of {MAX_BYTES}"
        )));
    }
    let mut bytes = vec![0; len + 1];
    input.read_exact(&mut bytes).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            ended()
        } else {
            e
        }
    })?;
    if bytes.pop() != Some(b'\n') {
        return Err(invalid("no newline after the bytes of a :cwd or :arg"));
    }
    Ok(bytes)
}

/// The error for input that ends inside a request.
fn ended() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the input ended inside a request",
    )
}

/// The error for input that is not a request, quoting `what`: the line, or the
/// part of a line, that is wrong, as far as it was read.
fn bad(what: &str) -> io::Error {
    invalid(format!("not an inkline mode request: {what:?}"))
}

/// The error for a request that breaks the protocol, saying how.
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_read_with_its_bytes_as_given() {
        let input = b":request 7\n:cwd 4\n/tmp\n:arg final 4\ncsvm\n:arg final 5\na\n\0|b\n\
                      :arg raw 7\n$HOME/x\n:arg final 0\n\n:done\n";
        let mut r = &input[..];
        assert_eq!(
            read_request(&mut r).unwrap(),
            Some(Request {
                id: 7,
                cwd: b"/tmp".to_vec(),
                args: vec![
                    Arg {
                        raw: false,
                        bytes: b"csvm".to_vec()
                    },
                    Arg {
                        raw: false,
                        bytes: b"a\n\0|b".to_vec()
                    },
                    Arg {
                        raw: true,
                        bytes: b"$HOME/x".to_vec()
                    },
                    Arg {
                        raw: false,
                        bytes: Vec::new()
                    },
                ],
                ask: Ask::Colors,
            })
        );
        // Then the input ends: no more requests.
        assert_eq!(read_request(&mut r).unwrap(), None);
    }

    #[test]
    fn input_that_ends_inside_a_request_is_one_error_wherever_it_ends() {
        let full = b":request 1\n:cwd 1\n/\n:arg final 4\ncsvm\n:done\n";
        for cut in 1..full.len() {
            let err = read_request(&mut &full[..cut]).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "cut at {cut}");
            assert_eq!(
                err.to_string(),
                "the input ended inside a request",
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn an_error_quotes_the_line_read_or_says_what_is_wrong() {
        let error = |input: &[u8]| read_request(&mut &input[..]).unwrap_err().to_string();
        // A line too long is quoted as far as it was read.
        let long = format!(":request {}", "1".repeat(300));
        assert_eq!(
            error(long.as_bytes()),
            format!(
                "not an inkline mode request: {:?}",
                &long[..MAX_LINE as usize]
            )
        );
        assert_eq!(
            error(b":request 1\n:cwd 99999999999\n"),
            format!("a :cwd or :arg of 99999999999 bytes is over the limit of {MAX_BYTES}")
        );
        assert_eq!(
            error(b":request 1\n:cwd 1\n/x:done\n"),
            "no newline after the bytes of a :cwd or :arg"
        );
    }

    #[test]
    fn a_request_that_breaks_the_protocol_is_an_error() {
        use io::ErrorKind::{InvalidData, UnexpectedEof};
        let bad = |input: &[u8]| {
            let mut r = input;
            read_request(&mut r).unwrap_err().kind()
        };
        assert_eq!(bad(b"hello\n"), InvalidData);
        assert_eq!(bad(b":request x\n"), InvalidData);
        assert_eq!(bad(b":request +1\n"), InvalidData);
        // A line cut off by the end of the input, or too long to be a `:` line.
        assert_eq!(bad(b":request 1"), UnexpectedEof);
        assert_eq!(
            bad(format!(":request {}\n", "1".repeat(300)).as_bytes()),
            InvalidData
        );
        // No `:cwd`, or a kind that is neither final nor raw.
        assert_eq!(bad(b":request 1\n:arg final 1\na\n"), InvalidData);
        assert_eq!(
            bad(b":request 1\n:cwd 1\n/\n:arg odd 1\na\n:done\n"),
            InvalidData
        );
        // Fewer bytes than the length says, or no newline after them.
        assert_eq!(bad(b":request 1\n:cwd 2\n/\n"), UnexpectedEof);
        assert_eq!(bad(b":request 1\n:cwd 1\n/x:done\n"), InvalidData);
        // A length too large to be a command line.
        assert_eq!(bad(b":request 1\n:cwd 99999999999\n"), InvalidData);
        // The input ends before `:done`.
        assert_eq!(bad(b":request 1\n:cwd 1\n/\n"), UnexpectedEof);
    }

    #[test]
    fn a_line_of_at_most_max_line_bytes_is_read() {
        // An id padded with zeros, so that only the line's length can fail.
        let with_line = |len: usize| {
            let digits = len - ":request \n".len();
            format!(":request {:0>digits$}\n:cwd 1\n/\n:done\n", 1)
        };
        let fits = with_line(MAX_LINE as usize);
        assert_eq!(read_request(&mut fits.as_bytes()).unwrap().unwrap().id, 1);
        let over = with_line(MAX_LINE as usize + 1);
        assert_eq!(
            read_request(&mut over.as_bytes()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn a_block_of_at_most_max_bytes_is_read() {
        let mut fits = format!(":request 1\n:cwd {MAX_BYTES}\n").into_bytes();
        fits.resize(fits.len() + MAX_BYTES, b'/');
        fits.extend_from_slice(b"\n:done\n");
        let request = read_request(&mut &fits[..]).unwrap().unwrap();
        assert_eq!(request.cwd.len(), MAX_BYTES);
        let over = format!(":request 1\n:cwd {}\n", MAX_BYTES + 1);
        assert_eq!(
            read_request(&mut over.as_bytes()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn an_indent_request_is_read_with_where_it_splits() {
        let input = b":indent 8\n:cwd 1\n/\n:arg final 4\ncsvm\n:arg final 7\njoin (\n\n\
                      :at 1 7\n:done\n";
        let mut r = &input[..];
        let request = read_request(&mut r).unwrap().unwrap();
        assert_eq!(request.id, 8);
        assert_eq!(request.args.len(), 2);
        assert_eq!(request.args[1].bytes, b"join (\n");
        assert_eq!(request.ask, Ask::Indent((1, 7)));
        assert_eq!(read_request(&mut r).unwrap(), None);
        // A split at the start of an argument, or in the command's name.
        let mut r = &b":indent 9\n:cwd 0\n\n:arg final 4\ncsvm\n:at 0 0\n:done\n"[..];
        assert_eq!(
            read_request(&mut r).unwrap().unwrap().ask,
            Ask::Indent((0, 0))
        );
    }

    #[test]
    fn an_indent_request_that_breaks_the_protocol_is_an_error() {
        use io::ErrorKind::InvalidData;
        let bad = |input: &[u8]| {
            let mut r = input;
            read_request(&mut r).unwrap_err().kind()
        };
        let head = ":indent 1\n:cwd 1\n/\n:arg final 4\ncsvm\n:arg final 3\nabc\n";
        let with = |tail: &str| format!("{head}{tail}").into_bytes();
        // No `:at`, which the error names, or more than one.
        assert_eq!(bad(&with(":done\n")), InvalidData);
        let no_at = with(":done\n");
        assert_eq!(
            read_request(&mut &no_at[..]).unwrap_err().to_string(),
            "an :indent request needs :at before :done"
        );
        assert_eq!(bad(&with(":at 1 0\n:at 1 1\n:done\n")), InvalidData);
        // Anything but `:done` after it.
        assert_eq!(bad(&with(":at 1 0\n:arg final 1\nx\n:done\n")), InvalidData);
        // Fields that are not two plain numbers.
        assert_eq!(bad(&with(":at 1\n:done\n")), InvalidData);
        assert_eq!(bad(&with(":at 1 -1\n:done\n")), InvalidData);
        assert_eq!(bad(&with(":at x 0\n:done\n")), InvalidData);
        assert_eq!(bad(&with(":at 1 0 0\n:done\n")), InvalidData);
        // Outside the arguments: no argument 2, and argument 1 has 3 bytes.
        assert_eq!(bad(&with(":at 2 0\n:done\n")), InvalidData);
        assert_eq!(bad(&with(":at 1 4\n:done\n")), InvalidData);
        // A colour request has no `:at`.
        assert_eq!(
            bad(b":request 1\n:cwd 1\n/\n:arg final 4\ncsvm\n:at 0 0\n:done\n"),
            InvalidData
        );
    }

    #[test]
    fn a_complete_request_is_read_with_its_place() {
        let input =
            ":complete 4\n:cwd 1\n/\n:arg final 4\ncsvm\n:arg final 2\nso\n:at 1 2\n:done\n";
        let request = read_request(&mut input.as_bytes()).unwrap().unwrap();
        assert_eq!(request.id, 4);
        assert_eq!(request.ask, Ask::Complete((1, 2)));
        // `:at` is needed, and must be inside the arguments.
        let no_at = ":complete 4\n:cwd 0\n\n:arg final 1\nx\n:done\n";
        assert!(read_request(&mut no_at.as_bytes()).is_err());
        let outside = ":complete 4\n:cwd 0\n\n:arg final 1\nx\n:at 0 5\n:done\n";
        assert!(read_request(&mut outside.as_bytes()).is_err());
    }

    fn request(cwd: &[u8], args: &[&[u8]]) -> Request {
        Request {
            id: 1,
            cwd: cwd.to_vec(),
            args: args
                .iter()
                .map(|a| Arg {
                    raw: false,
                    bytes: a.to_vec(),
                })
                .collect(),
            ask: Ask::Colors,
        }
    }

    #[test]
    fn the_words_are_the_arguments_after_the_name() {
        assert_eq!(
            request(b"/", &[b"\xff", b"a b", b""]).words(),
            Ok(vec!["a b".to_string(), String::new()])
        );
        // The first bytes that are not UTF-8, cut short or not.
        assert_eq!(
            request(b"/", &[b"c", b"ok", b"ab\xffcd\xff"]).words(),
            Err((2, 2..3))
        );
        assert_eq!(
            request(b"/", &[b"c", b"ab\xe2\x82"]).words(),
            Err((1, 2..4))
        );
    }

    #[test]
    fn the_shell_dir_is_an_absolute_utf8_path() {
        assert_eq!(request(b"/tmp", &[]).shell_dir(), Some(Path::new("/tmp")));
        assert_eq!(request(b"", &[]).shell_dir(), None);
        assert_eq!(request(b"tmp", &[]).shell_dir(), None);
        assert_eq!(request(b"/\xff", &[]).shell_dir(), None);
    }
}
