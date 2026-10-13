//! Writing a reply.

use std::io::{self, Write};
use std::ops::Range;

/// The most bytes of `:span` lines one reply may hold. inkline turns a mode
/// server off when a reply passes 1 MiB, so the spans past this are not sent
/// (the end of a very long argument is then not coloured), which leaves room
/// for the error and `:end`.
pub const MAX_SPAN_BYTES: usize = 900 << 10;

/// The most items one reply to a `:complete` request holds.
pub const MAX_ITEMS: usize = 1000;

/// The most bytes of `:item` and `:note` lines one reply holds, well under
/// inkline's 1 MiB limit, past which it turns a mode server off.
pub const MAX_ITEM_BYTES: usize = MAX_SPAN_BYTES;

/// The most bytes of an error's message or an item's note that are sent, and of
/// an item's text (a longer one is not sent at all), so that a message quoting
/// a very long word cannot push a reply past inkline's limit either.
pub const MAX_MESSAGE: usize = 4 << 10;

/// What a coloured part of an argument is: each kind is drawn with the
/// `inkline-colors` key of the same name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    Command,
    Keyword,
    Option,
    Operator,
    /// A mark between parts, such as the `|` between a pipeline's stages; drawn
    /// with the `operator` colour when the user has not set its own.
    Separator,
    String,
    Number,
    Variable,
    Function,
    Comment,
}

impl SpanKind {
    /// The kind's name in the protocol.
    pub fn name(self) -> &'static str {
        match self {
            SpanKind::Command => "command",
            SpanKind::Keyword => "keyword",
            SpanKind::Option => "option",
            SpanKind::Operator => "operator",
            SpanKind::Separator => "separator",
            SpanKind::String => "string",
            SpanKind::Number => "number",
            SpanKind::Variable => "variable",
            SpanKind::Function => "function",
            SpanKind::Comment => "comment",
        }
    }
}

/// The answer to one colour request.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reply {
    /// In order, none overlapping another.
    pub spans: Vec<Span>,
    pub error: Option<ReplyError>,
}

/// A coloured part of one argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The argument's index in the request.
    pub arg: usize,
    /// Byte offsets in the argument.
    pub at: Range<usize>,
    pub kind: SpanKind,
}

/// The one error a reply may carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyError {
    /// The argument, and the bytes of it, the error is about; `None` when it
    /// has no place in the arguments.
    pub place: Option<(usize, Range<usize>)>,
    /// One line of text.
    pub message: String,
}

impl ReplyError {
    /// An error with `message` made one line, as [`write_reply`] writes it.
    pub fn new(place: Option<(usize, Range<usize>)>, message: &str) -> ReplyError {
        ReplyError {
            place,
            message: one_line(message),
        }
    }
}

/// The answer to an `:indent` request: where two lines go, counted in steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Depths {
    /// The line that starts where the split is.
    pub new: usize,
    /// The line the split is on, when it moves too (such as one that starts
    /// with a closing bracket); `None` leaves that line as it is.
    pub current: Option<usize>,
}

/// One completion item, the answer to a `:complete` request: it replaces the
/// bytes `at` of the `:at` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub at: Range<usize>,
    pub text: String,
    pub note: Option<String>,
}

/// Write `reply` to request `id`, ending with `:end ID`. The spans stop before
/// they pass [`MAX_SPAN_BYTES`]; the error, on one line of at most
/// [`MAX_MESSAGE`] bytes, and the end are always written.
pub fn write_reply(out: &mut impl Write, id: u64, reply: &Reply) -> io::Result<()> {
    let mut written = 0;
    for span in &reply.spans {
        let line = format!(
            ":span {} {} {} {}\n",
            span.arg,
            span.at.start,
            span.at.end,
            span.kind.name()
        );
        written += line.len();
        if written > MAX_SPAN_BYTES {
            break;
        }
        out.write_all(line.as_bytes())?;
    }
    if let Some(error) = &reply.error {
        let message = one_line(&error.message);
        match &error.place {
            Some((arg, at)) => writeln!(out, ":error {arg} {} {} {message}", at.start, at.end)?,
            None => writeln!(out, ":error - - - {message}")?,
        }
    }
    writeln!(out, ":end {id}")
}

/// Write the answer to `:indent` request `id`: `:depth NEW CURRENT` when there
/// are depths, `CURRENT` being `-` when the line the split is on stays as it
/// is, then `:end ID`.
pub fn write_indent(out: &mut impl Write, id: u64, depths: Option<Depths>) -> io::Result<()> {
    if let Some(d) = depths {
        match d.current {
            Some(current) => writeln!(out, ":depth {} {current}", d.new)?,
            None => writeln!(out, ":depth {} -", d.new)?,
        }
    }
    writeln!(out, ":end {id}")
}

/// Write the answer to a `:complete` request `id`: each item as `:item START
/// END TEXT`, then `:note TEXT` when it has one, then `:end ID`. An item's text
/// is sent as it is, or not at all: an item whose text is empty, longer than
/// [`MAX_MESSAGE`] bytes, or holds a control character other than a tab is not
/// written (nor is its note). A note is made one line of at most
/// [`MAX_MESSAGE`] bytes. At most [`MAX_ITEMS`] items are written, and they
/// stop before their lines pass [`MAX_ITEM_BYTES`].
pub fn write_items(out: &mut impl Write, id: u64, items: &[Item]) -> io::Result<()> {
    for lines in written_items(items) {
        out.write_all(lines.as_bytes())?;
    }
    writeln!(out, ":end {id}")
}

/// Whether [`write_items`] writes every one of `items` that it does not leave
/// out for its text: none is cut by [`MAX_ITEMS`] or [`MAX_ITEM_BYTES`]. A
/// server that has more items than fit can put the ones it most wants sent
/// first, as [`wanted_first`] does.
pub fn items_fit(items: &[Item]) -> bool {
    written_items(items).count() == items.iter().filter(|i| sendable(&i.text)).count()
}

/// `items`, with the ones `wanted` picks first when they do not all fit (see
/// [`items_fit`]), so that the ones [`write_items`] cuts are the others. Each
/// group keeps its order, and so do `items` when they all fit.
pub fn wanted_first(items: Vec<Item>, wanted: impl FnMut(&Item) -> bool) -> Vec<Item> {
    if items_fit(&items) {
        return items;
    }
    let (mut first, rest): (Vec<Item>, Vec<Item>) = items.into_iter().partition(wanted);
    first.extend(rest);
    first
}

/// The lines [`write_items`] writes for `items`, one entry per item: the items
/// whose text is [`sendable`], at most [`MAX_ITEMS`] of them, stopping before
/// their lines pass [`MAX_ITEM_BYTES`].
fn written_items(items: &[Item]) -> impl Iterator<Item = String> {
    items
        .iter()
        .filter_map(item_lines)
        .take(MAX_ITEMS)
        .scan(0, |written, lines| {
            *written += lines.len();
            (*written <= MAX_ITEM_BYTES).then_some(lines)
        })
}

/// Whether an item's `text` can be sent as it is: not empty, at most
/// [`MAX_MESSAGE`] bytes, and no control character other than a tab.
fn sendable(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_MESSAGE
        && !text.chars().any(|c| c.is_control() && c != '\t')
}

/// The lines [`write_items`] writes for `item`: its `:item` line and its
/// `:note` line, if any. `None` when its text is not [`sendable`].
fn item_lines(item: &Item) -> Option<String> {
    let text = &item.text;
    if !sendable(text) {
        return None;
    }
    let mut lines = format!(":item {} {} {text}\n", item.at.start, item.at.end);
    if let Some(note) = &item.note {
        lines.push_str(&format!(":note {}\n", one_line(note)));
    }
    Some(lines)
}

/// `message` as one line: up to its first newline, at most [`MAX_MESSAGE`]
/// bytes, cut between two characters, a `\r` as a space, and no spaces at the
/// end.
fn one_line(message: &str) -> String {
    let first = message.split('\n').next().unwrap_or("");
    let first = &first[..first.floor_char_boundary(MAX_MESSAGE)];
    first.replace('\r', " ").trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_its_protocol_name() {
        let kinds = [
            SpanKind::Command,
            SpanKind::Keyword,
            SpanKind::Option,
            SpanKind::Operator,
            SpanKind::Separator,
            SpanKind::String,
            SpanKind::Number,
            SpanKind::Variable,
            SpanKind::Function,
            SpanKind::Comment,
        ];
        assert_eq!(
            kinds.map(SpanKind::name),
            [
                "command",
                "keyword",
                "option",
                "operator",
                "separator",
                "string",
                "number",
                "variable",
                "function",
                "comment"
            ]
        );
    }

    #[test]
    fn a_reply_is_written_line_by_line() {
        let reply = Reply {
            spans: vec![
                Span {
                    arg: 1,
                    at: 0..4,
                    kind: SpanKind::Command,
                },
                Span {
                    arg: 1,
                    at: 5..6,
                    kind: SpanKind::Variable,
                },
            ],
            error: Some(ReplyError::new(Some((1, 7..7)), "bad thing\nsecond line")),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 3, &reply).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            ":span 1 0 4 command\n:span 1 5 6 variable\n:error 1 7 7 bad thing\n:end 3\n"
        );
        // An error with no place; a stray `\r` does not reach the line.
        let reply = Reply {
            spans: Vec::new(),
            error: Some(ReplyError::new(None, "no place\r")),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 4, &reply).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            ":error - - - no place\n:end 4\n"
        );
        // Nothing to say: only the end.
        let mut out = Vec::new();
        write_reply(&mut out, 5, &Reply::default()).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), ":end 5\n");
    }

    /// inkline's reader requires the space before `MESSAGE` even when the
    /// message is empty (`:error ARG START END ` / `:error - - - `), so a
    /// consumer can always split off four fields. This checks the exact bytes
    /// for both an empty message with a place and one without.
    #[test]
    fn an_error_with_an_empty_message_still_gets_its_trailing_space() {
        let reply = Reply {
            spans: Vec::new(),
            error: Some(ReplyError::new(Some((0, 3..3)), "")),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 1, &reply).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), ":error 0 3 3 \n:end 1\n");

        let reply = Reply {
            spans: Vec::new(),
            error: Some(ReplyError::new(None, "")),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 2, &reply).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), ":error - - - \n:end 2\n");
    }

    #[test]
    fn a_message_is_written_on_one_line_however_it_was_built() {
        let reply = Reply {
            spans: Vec::new(),
            error: Some(ReplyError {
                place: None,
                message: "a\nb".to_string(),
            }),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 1, &reply).unwrap();
        assert_eq!(out, b":error - - - a\n:end 1\n");
        // A `\r` is a space; a message too long is cut on a character.
        let reply = Reply {
            spans: Vec::new(),
            error: Some(ReplyError {
                place: Some((1, 0..1)),
                message: format!("a\rb{}", "é".repeat(MAX_MESSAGE)),
            }),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 2, &reply).unwrap();
        let text = String::from_utf8(out).unwrap();
        let message = text
            .strip_prefix(":error 1 0 1 ")
            .and_then(|t| t.strip_suffix("\n:end 2\n"))
            .unwrap();
        assert!(message.starts_with("a b\u{e9}"));
        assert!(message.len() <= MAX_MESSAGE);
        assert!(message.len() > MAX_MESSAGE - 2);
    }

    /// Check that `bytes` is a well-formed reply to request `id`, by the
    /// protocol's shape: every line ends in a newline and is a `:span`,
    /// `:error` or `:end` line; a number field is plain ASCII digits, no sign;
    /// `:span` and `:error` each carry exactly four space-separated fields (so
    /// `MESSAGE`'s leading space is required even when it is empty); a span has
    /// `START < END` and a placed error `START <= END`; a placeless error is
    /// exactly `- - -`; there is at most one `:error`; and the reply ends with
    /// `:end ID`, naming the request's `ID`, and nothing after it. It does not
    /// know the arguments, so it cannot check an offset against an argument's
    /// length, nor that spans do not overlap, nor a span's `KIND`.
    fn check_reply_is_well_formed(bytes: &[u8], id: u64) {
        fn is_number(s: &str) -> bool {
            !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
        }

        let text = std::str::from_utf8(bytes).expect("reply must be UTF-8");
        let mut lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(
            lines.pop(),
            Some(""),
            "every line, :end included, ends in a newline"
        );
        assert!(!lines.is_empty(), "a reply always has :end");

        let mut error_seen = false;
        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.starts_with(':'),
                "a reply line starts with ':': {line:?}"
            );
            if let Some(rest) = line.strip_prefix(":span ") {
                let fields: Vec<&str> = rest.splitn(4, ' ').collect();
                assert_eq!(fields.len(), 4, ":span needs ARG START END KIND: {line:?}");
                assert!(is_number(fields[0]), "ARG is not plain digits: {line:?}");
                assert!(is_number(fields[1]), "START is not plain digits: {line:?}");
                assert!(is_number(fields[2]), "END is not plain digits: {line:?}");
                let start: u64 = fields[1].parse().unwrap();
                let end: u64 = fields[2].parse().unwrap();
                assert!(start < end, "a span needs START < END: {line:?}");
                assert!(!fields[3].is_empty(), "KIND is missing: {line:?}");
            } else if let Some(rest) = line.strip_prefix(":error ") {
                assert!(!error_seen, "at most one :error line per reply");
                error_seen = true;
                let fields: Vec<&str> = rest.splitn(4, ' ').collect();
                assert_eq!(
                    fields.len(),
                    4,
                    ":error needs ARG START END MESSAGE, MESSAGE's leading \
                     space included even when it is empty: {line:?}"
                );
                if fields[0] == "-" || fields[1] == "-" || fields[2] == "-" {
                    assert_eq!(
                        (fields[0], fields[1], fields[2]),
                        ("-", "-", "-"),
                        "a placeless error is exactly '- - -': {line:?}"
                    );
                } else {
                    assert!(is_number(fields[0]), "ARG is not plain digits: {line:?}");
                    assert!(is_number(fields[1]), "START is not plain digits: {line:?}");
                    assert!(is_number(fields[2]), "END is not plain digits: {line:?}");
                    let start: u64 = fields[1].parse().unwrap();
                    let end: u64 = fields[2].parse().unwrap();
                    assert!(start <= end, "an error needs START <= END: {line:?}");
                }
            } else if let Some(rest) = line.strip_prefix(":end ") {
                assert_eq!(i, lines.len() - 1, ":end must be the reply's last line");
                assert!(is_number(rest), "ID is not plain digits: {line:?}");
                assert_eq!(rest.parse::<u64>().unwrap(), id, "wrong :end ID");
            } else {
                panic!("not a reply line: {line:?}");
            }
        }
        assert!(
            lines.last().unwrap().starts_with(":end "),
            "a reply must end with :end"
        );
    }

    #[test]
    fn a_written_reply_is_well_formed() {
        let reply = Reply {
            spans: vec![
                Span {
                    arg: 1,
                    at: 0..4,
                    kind: SpanKind::Command,
                },
                Span {
                    arg: 1,
                    at: 5..6,
                    kind: SpanKind::Variable,
                },
            ],
            error: Some(ReplyError::new(Some((1, 7..7)), "bad thing")),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 9, &reply).unwrap();
        check_reply_is_well_formed(&out, 9);

        let reply = Reply {
            spans: Vec::new(),
            error: Some(ReplyError::new(None, "")),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 10, &reply).unwrap();
        check_reply_is_well_formed(&out, 10);

        let mut out = Vec::new();
        write_reply(&mut out, 11, &Reply::default()).unwrap();
        check_reply_is_well_formed(&out, 11);
    }

    #[test]
    fn a_reply_with_very_many_spans_stays_under_inklines_limit() {
        // Over 3 MiB of spans in all, and an error at the very end.
        let spans: Vec<Span> = (0..200_000)
            .map(|i| Span {
                arg: 1,
                at: 2 * i..2 * i + 1,
                kind: SpanKind::Variable,
            })
            .collect();
        let reply = Reply {
            spans,
            error: Some(ReplyError::new(Some((1, 400_000..400_005)), "at the end")),
        };
        let mut out = Vec::new();
        write_reply(&mut out, 1, &reply).unwrap();
        assert!(out.len() < 1 << 20, "{} bytes", out.len());
        check_reply_is_well_formed(&out, 1);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let [spans @ .., error, end] = &lines[..] else {
            panic!("too few lines");
        };
        // The spans written are the first ones, as many as fit.
        assert!(spans.len() > 20_000 && spans.len() < reply.spans.len());
        assert_eq!(spans[0], ":span 1 0 1 variable");
        let last = 2 * (spans.len() - 1);
        assert_eq!(
            spans[spans.len() - 1],
            format!(":span 1 {last} {} variable", last + 1)
        );
        // The error and the end are always written.
        assert_eq!(*error, ":error 1 400000 400005 at the end");
        assert_eq!(*end, ":end 1");
    }

    #[test]
    fn an_indent_reply_is_its_depths_and_the_end() {
        let mut out = Vec::new();
        let depths = Depths {
            new: 2,
            current: Some(1),
        };
        write_indent(&mut out, 4, Some(depths)).unwrap();
        assert_eq!(out, b":depth 2 1\n:end 4\n");
        // The line the split is on stays as it is.
        let mut out = Vec::new();
        let depths = Depths {
            new: 2,
            current: None,
        };
        write_indent(&mut out, 6, Some(depths)).unwrap();
        assert_eq!(out, b":depth 2 -\n:end 6\n");
        let mut out = Vec::new();
        write_indent(&mut out, 5, None).unwrap();
        assert_eq!(out, b":end 5\n");
    }

    /// Read `bytes` as inkline reads an indent reply to request `id`, and
    /// return its depths. inkline's rules: each line ends in a newline and
    /// splits at its first space into a keyword and the rest; a line that does
    /// not start with `:` is a failure; `:depth` comes at most once, and its
    /// rest is exactly two fields, one space apart, each a number made only of
    /// ASCII digits (no sign) that fits a `usize`, except that the second may
    /// be `-` instead (read as `None`); `:end`'s rest is such a number, equal
    /// to `id`, and ends the reply; any other `:` line is passed over.
    /// [`write_indent`] writes nothing after `:end`, so the reply must end
    /// there too.
    fn read_as_inkline(bytes: &[u8], id: u64) -> Option<(usize, Option<usize>)> {
        fn number<T: std::str::FromStr>(field: &str) -> T {
            assert!(
                !field.is_empty() && field.bytes().all(|b| b.is_ascii_digit()),
                "not plain digits: {field:?}"
            );
            field.parse().ok().expect("a number too large")
        }
        let text = std::str::from_utf8(bytes).expect("a reply in UTF-8");
        let body = text
            .strip_suffix('\n')
            .expect("the last line ends in a newline");
        let mut depths = None;
        let lines: Vec<&str> = body.split('\n').collect();
        for (i, line) in lines.iter().enumerate() {
            assert!(line.starts_with(':'), "not a reply line: {line:?}");
            let (keyword, rest) = line.split_once(' ').unwrap_or((line, ""));
            match keyword {
                ":depth" => {
                    assert!(depths.is_none(), "a second :depth: {line:?}");
                    let fields: Vec<&str> = rest.split(' ').collect();
                    let [new, current] = fields[..] else {
                        panic!(":depth needs two fields: {line:?}");
                    };
                    let current = (current != "-").then(|| number(current));
                    depths = Some((number(new), current));
                }
                ":end" => {
                    assert_eq!(number::<u64>(rest), id, "the wrong :end ID");
                    assert_eq!(i, lines.len() - 1, "a line after :end");
                    return depths;
                }
                _ => {}
            }
        }
        panic!("no :end");
    }

    #[test]
    fn an_indent_reply_reads_as_inkline_reads_it() {
        for (id, depths) in [
            (
                1,
                Some(Depths {
                    new: 0,
                    current: Some(0),
                }),
            ),
            (
                2,
                Some(Depths {
                    new: 3,
                    current: Some(12),
                }),
            ),
            (3, None),
            (
                u64::MAX,
                Some(Depths {
                    new: usize::MAX,
                    current: Some(1),
                }),
            ),
            (
                4,
                Some(Depths {
                    new: 1,
                    current: None,
                }),
            ),
        ] {
            let mut out = Vec::new();
            write_indent(&mut out, id, depths).unwrap();
            assert_eq!(
                read_as_inkline(&out, id),
                depths.map(|d| (d.new, d.current))
            );
        }
    }

    #[test]
    fn items_are_written_one_per_line_with_their_notes() {
        let mut out = Vec::new();
        let items = [
            Item {
                at: 5..7,
                text: "amount".into(),
                note: Some("column".into()),
            },
            Item {
                at: 5..7,
                text: "first name".into(),
                note: None,
            },
            // Not one line: not written, nor its note.
            Item {
                at: 5..7,
                text: "a\nb".into(),
                note: Some("gone".into()),
            },
            // A note is made one line.
            Item {
                at: 0..0,
                text: "ab".into(),
                note: Some("c\rd\ne".into()),
            },
        ];
        write_items(&mut out, 9, &items).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            ":item 5 7 amount\n:note column\n:item 5 7 first name\n\
             :item 0 0 ab\n:note c d\n:end 9\n"
        );
    }

    #[test]
    fn an_item_is_sent_as_it_is_or_not_at_all() {
        let item = |text: &str| Item {
            at: 0..0,
            text: text.into(),
            note: None,
        };
        let sent = |text: &str| {
            let mut out = Vec::new();
            write_items(&mut out, 1, &[item(text)]).unwrap();
            let out = String::from_utf8(out).unwrap();
            out.strip_prefix(":item 0 0 ")
                .and_then(|o| o.strip_suffix("\n:end 1\n"))
                .map(str::to_string)
        };
        // Spaces at the end and tabs are part of the text.
        assert_eq!(sent("a b "), Some("a b ".into()));
        assert_eq!(sent("a\tb"), Some("a\tb".into()));
        assert_eq!(
            sent(&"x".repeat(MAX_MESSAGE)),
            Some("x".repeat(MAX_MESSAGE))
        );
        // Text that inkline would not take as it is.
        assert_eq!(sent(""), None);
        assert_eq!(sent("a\nb"), None);
        assert_eq!(sent("a\rb"), None);
        assert_eq!(sent("a\x1bb"), None);
        assert_eq!(sent(&"x".repeat(MAX_MESSAGE + 1)), None);
        // Items left out do not count against the limits.
        let mut items = numbered(MAX_ITEMS, 4);
        items.push(item("a\nb"));
        assert!(items_fit(&items));
    }

    fn numbered(n: usize, len: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                at: 0..0,
                text: format!("{i:0len$}"),
                note: Some("column".into()),
            })
            .collect()
    }

    #[test]
    fn a_reply_holds_at_most_so_many_items_and_bytes() {
        let written = |items: &[Item]| {
            let mut out = Vec::new();
            write_items(&mut out, 1, items).unwrap();
            let out = String::from_utf8(out).unwrap();
            assert!(out.ends_with(":end 1\n"));
            let bytes = out.len() - ":end 1\n".len();
            (
                out.lines().filter(|l| l.starts_with(":item")).count(),
                bytes,
            )
        };
        let (count, bytes) = written(&numbered(MAX_ITEMS + 5, 4));
        assert_eq!(count, MAX_ITEMS);
        assert!(bytes <= MAX_ITEM_BYTES);
        let (count, bytes) = written(&numbered(500, 4000));
        assert!(count < 500);
        assert!(bytes <= MAX_ITEM_BYTES && bytes > MAX_ITEM_BYTES - 4100);
    }

    #[test]
    fn the_wanted_items_come_first_when_not_all_fit() {
        let odd = |item: &Item| item.text.ends_with(['1', '3', '5', '7', '9']);
        // All fit: the order is kept.
        let items = numbered(4, 4);
        assert_eq!(wanted_first(items.clone(), odd), items);
        // Not all fit: the wanted ones first, each group in its own order.
        let items = numbered(MAX_ITEMS + 2, 4);
        let ordered = wanted_first(items.clone(), odd);
        let (wanted, rest): (Vec<Item>, Vec<Item>) = items.into_iter().partition(odd);
        assert_eq!(ordered, [wanted, rest].concat());
    }

    #[test]
    fn items_fit_when_none_is_cut() {
        assert!(items_fit(&[]));
        assert!(items_fit(&numbered(MAX_ITEMS, 4)));
        assert!(!items_fit(&numbered(MAX_ITEMS + 1, 4)));
        assert!(!items_fit(&numbered(500, 4000)));
        // Items left out for their text do not count.
        let mut items = numbered(MAX_ITEMS, 4);
        items.push(Item {
            at: 0..0,
            text: String::new(),
            note: None,
        });
        assert!(items_fit(&items));
    }
}
