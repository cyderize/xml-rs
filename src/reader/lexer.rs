//! Contains simple lexer for XML documents.
//!
//! This module is for internal use. Use `xml::pull` module to do parsing.

use std::mem;
use std::fmt;
use std::string::ToString;

use common::{Error, HasPosition, is_whitespace_char, is_name_char};

/// `Token` represents a single lexeme of an XML document. These lexemes
/// are used to perform actual parsing.
#[derive(Clone, PartialEq)]
pub enum Token {
    /// `<?`
    ProcessingInstructionStart,
    /// `?>`
    ProcessingInstructionEnd,
    /// `<!DOCTYPE
    DoctypeStart,
    /// `<`
    OpeningTagStart,
    /// `</`
    ClosingTagStart,
    /// `>`
    TagEnd,
    /// `/>`
    EmptyTagEnd,
    /// `<!--`
    CommentStart,
    /// `-->`
    CommentEnd,
    /// A chunk of characters, used for errors recovery.
    Chunk(&'static str),
    /// Any non-special character except whitespace.
    Character(char),
    /// Whitespace character.
    Whitespace(char),
    /// `=`
    EqualsSign,
    /// `'`
    SingleQuote,
    /// `"`
    DoubleQuote,
    /// `<![CDATA[`
    CDataStart,
    /// `]]>`
    CDataEnd,
    /// `&`
    ReferenceStart,
    /// `;`
    ReferenceEnd,
}

impl fmt::String for Token {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Token::Chunk(s)                            => write!(f, "{}", s),
            Token::Character(c) | Token::Whitespace(c) => write!(f, "{}", c),
            ref other => write!(f, "{}", match *other {
                Token::OpeningTagStart            => "<",
                Token::ProcessingInstructionStart => "<?",
                Token::DoctypeStart               => "<!DOCTYPE",
                Token::ClosingTagStart            => "</",
                Token::CommentStart               => "<!--",
                Token::CDataStart                 => "<![CDATA[",
                Token::TagEnd                     => ">",
                Token::EmptyTagEnd                => "/>",
                Token::ProcessingInstructionEnd   => "?>",
                Token::CommentEnd                 => "-->",
                Token::CDataEnd                   => "]]>",
                Token::ReferenceStart             => "&",
                Token::ReferenceEnd               => ";",
                Token::EqualsSign                 => "=",
                Token::SingleQuote                => "'",
                Token::DoubleQuote                => "\"",
                _                          => unreachable!()
            })
        }
    }
}

impl fmt::Show for Token {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::String::fmt(self, f)
    }
}

impl Token {
    pub fn as_static_str(&self) -> Option<&'static str> {
        match *self {
            Token::OpeningTagStart            => Some("<"),
            Token::ProcessingInstructionStart => Some("<?"),
            Token::DoctypeStart               => Some("<!DOCTYPE"),
            Token::ClosingTagStart            => Some("</"),
            Token::CommentStart               => Some("<!--"),
            Token::CDataStart                 => Some("<![CDATA["),
            Token::TagEnd                     => Some(">"),
            Token::EmptyTagEnd                => Some("/>"),
            Token::ProcessingInstructionEnd   => Some("?>"),
            Token::CommentEnd                 => Some("-->"),
            Token::CDataEnd                   => Some("]]>"),
            Token::ReferenceStart             => Some("&"),
            Token::ReferenceEnd               => Some(";"),
            Token::EqualsSign                 => Some("="),
            Token::SingleQuote                => Some("'"),
            Token::DoubleQuote                => Some("\""),
            _                                 => None
        }
    }

    /// Returns `true` if this token contains data that can be interpreted
    /// as a part of the text. Surprisingly, this also means '>' and '=' and '"' and "'".
    #[inline]
    pub fn contains_char_data(&self) -> bool {
        match *self {
            Token::Whitespace(_) | Token::Chunk(_) | Token::Character(_) |
            Token::TagEnd | Token::EqualsSign | Token::DoubleQuote | Token::SingleQuote => true,
            _ => false
        }
    }

    /// Returns `true` if this token corresponds to a white space character.
    #[inline]
    pub fn is_whitespace(&self) -> bool {
        match *self {
            Token::Whitespace(_) => true,
            _ => false
        }
    }
}

enum State {
    /// Triggered on '<'
    TagOpened,
    /// Triggered on '<!'
    CommentOrCDataOrDoctypeStarted,
    /// Triggered on '<!-'
    CommentStarted,
    /// Triggered on '<!D' up to '<!DOCTYPE'
    DoctypeStarted(DoctypeStartedSubstate),
    /// Triggered on '<![' up to '<![CDATA'
    CDataStarted(CDataStartedSubstate),
    /// Triggered on '?'
    ProcessingInstructionClosing,
    /// Triggered on '/'
    EmptyTagClosing,
    /// Triggered on '-' up to '--'
    CommentClosing(ClosingSubstate),
    /// Triggered on ']' up to ']]'
    CDataClosing(ClosingSubstate),
    /// Default state
    Normal
}

#[derive(Copy)]
enum ClosingSubstate {
    First, Second
}

#[derive(Copy)]
enum DoctypeStartedSubstate {
    D, DO, DOC, DOCT, DOCTY, DOCTYP
}

#[derive(Copy)]
enum CDataStartedSubstate {
    E, C, CD, CDA, CDAT, CDATA
}

/// `LexResult` represents lexing result. It is either a token or an error message.
pub type LexResult = Result<Token, Error>;

type LexStep = Option<LexResult>;  // TODO: make up with better name

/// Helps to set up a dispatch table for lexing large unambigous tokens like
/// `<![CDATA[` or `<!DOCTYPE `.
macro_rules! dispatch_on_enum_state(
    ($_self:ident, $s:expr, $c:expr, $is:expr,
     $($st:ident; $stc:expr ; $next_st:ident ; $chunk:expr),+;
     $end_st:ident ; $end_c:expr ; $end_chunk:expr ; $e:expr) => (
        match $s {
            $(
            $st => match $c {
                $stc => $_self.move_to($is($next_st)),
                _  => $_self.handle_error($chunk, $c)
            },
            )+
            $end_st => match $c {
                $end_c => $e,
                _      => $_self.handle_error($end_chunk, $c)
            }
        }
    )
);

/// `PullLexer` is a lexer for XML documents, which implements pull API.
///
/// Main method is `next_token` which accepts an `std::io::Buffer` and
/// tries to read the next lexeme from it.
///
/// When `skip_errors` flag is set, invalid lexemes will be returned as `Chunk`s.
/// When it is not set, errors will be reported as `Err` objects with a string message.
/// By default this flag is not set. Use `enable_errors` and `disable_errors` methods
/// to toggle the behavior.
pub struct PullLexer {
    row: usize,
    col: usize,
    temp_char: Option<char>,
    st: State,
    skip_errors: bool,
    eof_handled: bool
}

/// Returns a new lexer with default state.
pub fn new() -> PullLexer {
    PullLexer {
        row: 0,
        col: 0,
        temp_char: None,
        st: State::Normal,
        skip_errors: false,
        eof_handled: false
    }
}

impl HasPosition for PullLexer {
    /// Returns current row in the input document.
    #[inline]
    fn row(&self) -> usize { self.row }

    /// Returns current column in the document.
    #[inline]
    fn col(&self) -> usize { self.col }
}

impl PullLexer {
    /// Enables error handling so `next_token` will return `Some(Err(..))`
    /// upon invalid lexeme.
    #[inline]
    pub fn enable_errors(&mut self) { self.skip_errors = false; }

    /// Disables error handling so `next_token` will return `Some(Chunk(..))`
    /// upon invalid lexeme with this lexeme content.
    #[inline]
    pub fn disable_errors(&mut self) { self.skip_errors = true; }

    /// Tries to read next token from the buffer.
    ///
    /// It is possible to pass different instaces of `Buffer` each time
    /// this method is called, but the resulting behavior is undefined.
    ///
    /// Returns `None` when logical end of stream is encountered, that is,
    /// after `b.read_char()` returns `None` and the current state is
    /// is exhausted.
    pub fn next_token<B: Buffer>(&mut self, b: &mut B) -> Option<LexResult> {
        // Already reached end of buffer
        if self.eof_handled {
            return None;
        }

        // Check if we have saved a char for ourselves
        if self.temp_char.is_some() {
            let c = mem::replace(&mut self.temp_char, None).unwrap();
            match self.read_next_token(c) {
                Some(t) => return Some(t),
                None => {}  // continue
            }
        }

        // Read more data from the buffer
        for_each!(c in b.read_char().ok() ; {
            match self.read_next_token(c) {
                Some(t) => return Some(t),
                None    => {}  // continue
            }
        });

        // Handle end of stream
        self.eof_handled = true;
        match self.st {
            State::TagOpened | State::CommentOrCDataOrDoctypeStarted |
            State::CommentStarted | State::CDataStarted(_)| State::DoctypeStarted(_) |
            State::CommentClosing(ClosingSubstate::Second)  =>
                Some(Err(self.error("Unexpected end of stream"))),
            State::ProcessingInstructionClosing =>
                Some(Ok(Token::Character('?'))),
            State::EmptyTagClosing =>
                Some(Ok(Token::Character('/'))),
            State::CommentClosing(ClosingSubstate::First) =>
                Some(Ok(Token::Character('-'))),
            State::CDataClosing(ClosingSubstate::First) =>
                Some(Ok(Token::Character(']'))),
            State::CDataClosing(ClosingSubstate::Second) =>
                Some(Ok(Token::Chunk("]]"))),
            State::Normal =>
                None
        }
    }

    #[inline]
    fn error(&self, msg: &str) -> Error {
        Error::new(self, msg.to_string())
    }

    fn read_next_token(&mut self, c: char) -> LexStep {
        if c == '\n' {
            self.row += 1;
            self.col = 0;
        } else {
            self.col += 1;
        }

        self.dispatch_char(c)
    }


    fn dispatch_char(&mut self, c: char) -> LexStep {
        match self.st {
            State::Normal                         => self.normal(c),
            State::TagOpened                      => self.tag_opened(c),
            State::CommentOrCDataOrDoctypeStarted => self.comment_or_cdata_or_doctype_started(c),
            State::CommentStarted                 => self.comment_started(c),
            State::CDataStarted(s)                => self.cdata_started(c, s),
            State::DoctypeStarted(s)              => self.doctype_started(c, s),
            State::ProcessingInstructionClosing   => self.processing_instruction_closing(c),
            State::EmptyTagClosing                => self.empty_element_closing(c),
            State::CommentClosing(s)              => self.comment_closing(c, s),
            State::CDataClosing(s)                => self.cdata_closing(c, s)
        }
    }

    #[inline]
    fn move_to(&mut self, st: State) -> LexStep {
        self.st = st;
        None
    }

    #[inline]
    fn move_to_with(&mut self, st: State, token: Token) -> LexStep {
        self.st = st;
        Some(Ok(token))
    }

    #[inline]
    fn move_to_with_unread(&mut self, st: State, c: char, token: Token) -> LexStep {
        self.temp_char = Some(c);
        self.move_to_with(st, token)
    }

    fn handle_error(&mut self, chunk: &'static str, c: char) -> LexStep {
        self.temp_char = Some(c);
        if self.skip_errors {
            self.move_to_with(State::Normal, Token::Chunk(chunk))
        } else {
            Some(Err(
                Error::new_full(
                    self.row, self.col-chunk.len()-1,
                    format!("Unexpected token {} before {}", chunk, c)
                )
            ))
        }
    }

    /// Encountered a char
    fn normal(&mut self, c: char) -> LexStep {
        match c {
            '<'                        => self.move_to(State::TagOpened),
            '>'                        => Some(Ok(Token::TagEnd)),
            '/'                        => self.move_to(State::EmptyTagClosing),
            '='                        => Some(Ok(Token::EqualsSign)),
            '"'                        => Some(Ok(Token::DoubleQuote)),
            '\''                       => Some(Ok(Token::SingleQuote)),
            '?'                        => self.move_to(State::ProcessingInstructionClosing),
            '-'                        => self.move_to(State::CommentClosing(ClosingSubstate::First)),
            ']'                        => self.move_to(State::CDataClosing(ClosingSubstate::First)),
            '&'                        => Some(Ok(Token::ReferenceStart)),
            ';'                        => Some(Ok(Token::ReferenceEnd)),
            _ if is_whitespace_char(c) => Some(Ok(Token::Whitespace(c))),
            _                          => Some(Ok(Token::Character(c)))
        }
    }

    /// Encountered '<'
    fn tag_opened(&mut self, c: char) -> LexStep {
        match c {
            '?'                        => self.move_to_with(State::Normal, Token::ProcessingInstructionStart),
            '/'                        => self.move_to_with(State::Normal, Token::ClosingTagStart),
            '!'                        => self.move_to(State::CommentOrCDataOrDoctypeStarted),
            _ if is_whitespace_char(c) => self.move_to_with_unread(State::Normal, c, Token::OpeningTagStart),
            _ if is_name_char(c)       => self.move_to_with_unread(State::Normal, c, Token::OpeningTagStart),
            _                          => self.handle_error("<", c)
        }
    }

    /// Encountered '<!'
    fn comment_or_cdata_or_doctype_started(&mut self, c: char) -> LexStep {
        match c {
            '-' => self.move_to(State::CommentStarted),
            '[' => self.move_to(State::CDataStarted(CDataStartedSubstate::E)),
            'D' => self.move_to(State::DoctypeStarted(DoctypeStartedSubstate::D)),
            _   => self.handle_error("<!", c)
        }
    }

    /// Encountered '<!-'
    fn comment_started(&mut self, c: char) -> LexStep {
        match c {
            '-' => self.move_to_with(State::Normal, Token::CommentStart),
            _   => self.handle_error("<!-", c)
        }
    }

    /// Encountered '<!['
    fn cdata_started(&mut self, c: char, s: CDataStartedSubstate) -> LexStep {
        use self::CDataStartedSubstate::{E, C, CD, CDA, CDAT, CDATA};
        dispatch_on_enum_state!(self, s, c, State::CDataStarted,
            E     ; 'C' ; C     ; "<![",
            C     ; 'D' ; CD    ; "<![C",
            CD    ; 'A' ; CDA   ; "<![CD",
            CDA   ; 'T' ; CDAT  ; "<![CDA",
            CDAT  ; 'A' ; CDATA ; "<![CDAT";
            CDATA ; '[' ; "<![CDATA" ; self.move_to_with(State::Normal, Token::CDataStart)
        )
    }

    /// Encountered '<!D'
    fn doctype_started(&mut self, c: char, s: DoctypeStartedSubstate) -> LexStep {
        use self::DoctypeStartedSubstate::{D, DO, DOC, DOCT, DOCTY, DOCTYP};
        dispatch_on_enum_state!(self, s, c, State::DoctypeStarted,
            D      ; 'O' ; DO     ; "<!D",
            DO     ; 'C' ; DOC    ; "<!DO",
            DOC    ; 'T' ; DOCT   ; "<!DOC",
            DOCT   ; 'Y' ; DOCTY  ; "<!DOCT",
            DOCTY  ; 'P' ; DOCTYP ; "<!DOCTY";
            DOCTYP ; 'E' ; "<!DOCTYP" ; self.move_to_with(State::Normal, Token::DoctypeStart)
        )
    }

    /// Encountered '?'
    fn processing_instruction_closing(&mut self, c: char) -> LexStep {
        match c {
            '>' => self.move_to_with(State::Normal, Token::ProcessingInstructionEnd),
            _   => self.move_to_with_unread(State::Normal, c, Token::Character('?')),
        }
    }

    /// Encountered '/'
    fn empty_element_closing(&mut self, c: char) -> LexStep {
        match c {
            '>' => self.move_to_with(State::Normal, Token::EmptyTagEnd),
            _   => self.move_to_with_unread(State::Normal, c, Token::Character('/')),
        }
    }

    /// Encountered '-'
    fn comment_closing(&mut self, c: char, s: ClosingSubstate) -> LexStep {
        match s {
            ClosingSubstate::First => match c {
                '-' => self.move_to(State::CommentClosing(ClosingSubstate::Second)),
                _   => self.move_to_with_unread(State::Normal, c, Token::Character('-'))
            },
            ClosingSubstate::Second => match c {
                '>' => self.move_to_with(State::Normal, Token::CommentEnd),
                _   => self.move_to_with_unread(State::Normal, c, Token::Chunk("--"))
            }
        }
    }

    /// Encountered ']'
    fn cdata_closing(&mut self, c: char, s: ClosingSubstate) -> LexStep {
        match s {
            ClosingSubstate::First => match c {
                ']' => self.move_to(State::CDataClosing(ClosingSubstate::Second)),
                _   => self.move_to_with_unread(State::Normal, c, Token::Character(']'))
            },
            ClosingSubstate::Second => match c {
                '>' => self.move_to_with(State::Normal, Token::CDataEnd),
                _   => self.move_to_with_unread(State::Normal, c, Token::Chunk("]]"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::MemReader;

    use common::{HasPosition};

    use super::{PullLexer, Token};

    macro_rules! assert_oks(
        (for $lex:ident and $buf:ident ; $($e:expr)+) => ({
            $(
                assert_eq!(Some(Ok($e)), $lex.next_token(&mut $buf));
             )+
        })
    );

    macro_rules! assert_err(
        (for $lex:ident and $buf:ident expect row $r:expr ; $c:expr, $s:expr) => ({
            let err = $lex.next_token(&mut $buf);
            assert!(err.is_some());
            assert!(err.as_ref().unwrap().is_err());
            let err = err.unwrap().unwrap_err();
            assert_eq!($r as usize, err.row());
            assert_eq!($c as usize, err.col());
            assert_eq!($s, err.msg());
        })
    );

    macro_rules! assert_none(
        (for $lex:ident and $buf:ident) => (
            assert_eq!(None, $lex.next_token(&mut $buf));
        )
    );

    fn make_buf(s: String) -> MemReader {
        MemReader::new(s.into_bytes())
    }

    fn make_lex_and_buf(s: &str) -> (PullLexer, MemReader) {
        (super::new(), make_buf(s.to_string()))
    }

    #[test]
    fn simple_lexer_test() {
        let (mut lex, mut buf) = make_lex_and_buf(
            r#"<a p='q'> x<b z="y">d	</b></a><p/> <?nm ?> <!-- a c --> &nbsp;"#
        );

        assert_oks!(for lex and buf ;
            Token::OpeningTagStart
            Token::Character('a')
            Token::Whitespace(' ')
            Token::Character('p')
            Token::EqualsSign
            Token::SingleQuote
            Token::Character('q')
            Token::SingleQuote
            Token::TagEnd
            Token::Whitespace(' ')
            Token::Character('x')
            Token::OpeningTagStart
            Token::Character('b')
            Token::Whitespace(' ')
            Token::Character('z')
            Token::EqualsSign
            Token::DoubleQuote
            Token::Character('y')
            Token::DoubleQuote
            Token::TagEnd
            Token::Character('d')
            Token::Whitespace('\t')
            Token::ClosingTagStart
            Token::Character('b')
            Token::TagEnd
            Token::ClosingTagStart
            Token::Character('a')
            Token::TagEnd
            Token::OpeningTagStart
            Token::Character('p')
            Token::EmptyTagEnd
            Token::Whitespace(' ')
            Token::ProcessingInstructionStart
            Token::Character('n')
            Token::Character('m')
            Token::Whitespace(' ')
            Token::ProcessingInstructionEnd
            Token::Whitespace(' ')
            Token::CommentStart
            Token::Whitespace(' ')
            Token::Character('a')
            Token::Whitespace(' ')
            Token::Character('c')
            Token::Whitespace(' ')
            Token::CommentEnd
            Token::Whitespace(' ')
            Token::ReferenceStart
            Token::Character('n')
            Token::Character('b')
            Token::Character('s')
            Token::Character('p')
            Token::ReferenceEnd
        );
        assert_none!(for lex and buf);
    }

    #[test]
    fn special_chars_test() {
        let (mut lex, mut buf) = make_lex_and_buf(
            r#"?x!+ // -| ]z]]"#
        );

        assert_oks!(for lex and buf ;
            Token::Character('?')
            Token::Character('x')
            Token::Character('!')
            Token::Character('+')
            Token::Whitespace(' ')
            Token::Character('/')
            Token::Character('/')
            Token::Whitespace(' ')
            Token::Character('-')
            Token::Character('|')
            Token::Whitespace(' ')
            Token::Character(']')
            Token::Character('z')
            Token::Chunk("]]")
        );
        assert_none!(for lex and buf);
    }

    #[test]
    fn cdata_test() {
        let (mut lex, mut buf) = make_lex_and_buf(
            r#"<a><![CDATA[x y ?]]> </a>"#
        );

        assert_oks!(for lex and buf ;
            Token::OpeningTagStart
            Token::Character('a')
            Token::TagEnd
            Token::CDataStart
            Token::Character('x')
            Token::Whitespace(' ')
            Token::Character('y')
            Token::Whitespace(' ')
            Token::Character('?')
            Token::CDataEnd
            Token::Whitespace(' ')
            Token::ClosingTagStart
            Token::Character('a')
            Token::TagEnd
        );
        assert_none!(for lex and buf);
    }

    #[test]
    fn doctype_test() {
        let (mut lex, mut buf) = make_lex_and_buf(
            r#"<a><!DOCTYPE ab xx z> "#
        );
        assert_oks!(for lex and buf ;
            Token::OpeningTagStart
            Token::Character('a')
            Token::TagEnd
            Token::DoctypeStart
            Token::Whitespace(' ')
            Token::Character('a')
            Token::Character('b')
            Token::Whitespace(' ')
            Token::Character('x')
            Token::Character('x')
            Token::Whitespace(' ')
            Token::Character('z')
            Token::TagEnd
            Token::Whitespace(' ')
        );
        assert_none!(for lex and buf)
    }

    #[test]
    fn end_of_stream_handling_ok() {
        macro_rules! eof_check(
            ($data:expr ; $token:expr) => ({
                let (mut lex, mut buf) = make_lex_and_buf($data);
                assert_oks!(for lex and buf ; $token);
                assert_none!(for lex and buf);
            })
        );
        eof_check!("?"  ; Token::Character('?'));
        eof_check!("/"  ; Token::Character('/'));
        eof_check!("-"  ; Token::Character('-'));
        eof_check!("]"  ; Token::Character(']'));
        eof_check!("]]" ; Token::Chunk("]]"));
    }

    #[test]
    fn end_of_stream_handling_error() {
        macro_rules! eof_check(
            ($data:expr; $r:expr, $c:expr) => ({
                let (mut lex, mut buf) = make_lex_and_buf($data);
                assert_err!(for lex and buf expect row $r ; $c, "Unexpected end of stream");
                assert_none!(for lex and buf);
            })
        );
        eof_check!("<"        ; 0, 1);
        eof_check!("<!"       ; 0, 2);
        eof_check!("<!-"      ; 0, 3);
        eof_check!("<!["      ; 0, 3);
        eof_check!("<![C"     ; 0, 4);
        eof_check!("<![CD"    ; 0, 5);
        eof_check!("<![CDA"   ; 0, 6);
        eof_check!("<![CDAT"  ; 0, 7);
        eof_check!("<![CDATA" ; 0, 8);
        eof_check!("--"       ; 0, 2);
    }

    #[test]
    fn error_in_comment_or_cdata_prefix() {
        let (mut lex, mut buf) = make_lex_and_buf("<!x");
        assert_err!(for lex and buf expect row 0 ; 0,
            "Unexpected token <! before x"
        );

        let (mut lex, mut buf) = make_lex_and_buf("<!x");
        lex.disable_errors();
        assert_oks!(for lex and buf ;
            Token::Chunk("<!")
            Token::Character('x')
        );
        assert_none!(for lex and buf);
    }

    #[test]
    fn error_in_comment_started() {
        let (mut lex, mut buf) = make_lex_and_buf("<!-\t");
        assert_err!(for lex and buf expect row 0 ; 0,
            "Unexpected token <!- before \t"
        );

        let (mut lex, mut buf) = make_lex_and_buf("<!-\t");
        lex.disable_errors();
        assert_oks!(for lex and buf ;
            Token::Chunk("<!-")
            Token::Whitespace('\t')
        );
        assert_none!(for lex and buf);
    }

    macro_rules! check_case(
        ($chunk:expr, $app:expr; $data:expr; $r:expr, $c:expr, $s:expr) => ({
            let (mut lex, mut buf) = make_lex_and_buf($data);
            assert_err!(for lex and buf expect row $r ; $c, $s);

            let (mut lex, mut buf) = make_lex_and_buf($data);
            lex.disable_errors();
            assert_oks!(for lex and buf ;
                Token::Chunk($chunk)
                Token::Character($app)
            );
            assert_none!(for lex and buf);
        })
    );

    #[test]
    fn error_in_cdata_started() {
        check_case!("<![",      '['; "<![["      ; 0, 0, "Unexpected token <![ before [");
        check_case!("<![C",     '['; "<![C["     ; 0, 0, "Unexpected token <![C before [");
        check_case!("<![CD",    '['; "<![CD["    ; 0, 0, "Unexpected token <![CD before [");
        check_case!("<![CDA",   '['; "<![CDA["   ; 0, 0, "Unexpected token <![CDA before [");
        check_case!("<![CDAT",  '['; "<![CDAT["  ; 0, 0, "Unexpected token <![CDAT before [");
        check_case!("<![CDATA", '|'; "<![CDATA|" ; 0, 0, "Unexpected token <![CDATA before |");
    }

    #[test]
    fn error_in_doctype_started() {
        check_case!("<!D",      'a'; "<!Da"      ; 0, 0, "Unexpected token <!D before a");
        check_case!("<!DO",     'b'; "<!DOb"     ; 0, 0, "Unexpected token <!DO before b");
        check_case!("<!DOC",    'c'; "<!DOCc"    ; 0, 0, "Unexpected token <!DOC before c");
        check_case!("<!DOCT",   'd'; "<!DOCTd"   ; 0, 0, "Unexpected token <!DOCT before d");
        check_case!("<!DOCTY",  'e'; "<!DOCTYe"  ; 0, 0, "Unexpected token <!DOCTY before e");
        check_case!("<!DOCTYP", 'f'; "<!DOCTYPf" ; 0, 0, "Unexpected token <!DOCTYP before f");
    }
}
