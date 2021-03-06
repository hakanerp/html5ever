// Copyright 2014-2017 The html5ever Project Developers. See the
// COPYRIGHT file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use tokenizer::{XmlTokenizerOpts, XmlTokenizer};
use tree_builder::{TreeSink, XmlTreeBuilder, XmlTreeBuilderOpts};

use std::borrow::Cow;
use std::mem;

use encoding::{self, EncodingRef};
use tendril;
use tendril::{StrTendril, ByteTendril};
use tendril::stream::{TendrilSink, Utf8LossyDecoder, LossyDecoder};

/// All-encompasing parser setting structure.
#[derive(Clone, Default)]
pub struct XmlParseOpts {
    /// Xml tokenizer options.
    pub tokenizer: XmlTokenizerOpts,
    /// Xml tree builder .
    pub tree_builder: XmlTreeBuilderOpts,
}

/// Parse and send results to a `TreeSink`.
///
/// ## Example
///
/// ```ignore
/// let mut sink = MySink;
/// parse_document(&mut sink, iter::once(my_str), Default::default());
/// ```
pub fn parse_document<Sink>(sink: Sink, opts: XmlParseOpts) -> XmlParser<Sink>
    where Sink: TreeSink {

    let tb = XmlTreeBuilder::new(sink, opts.tree_builder);
    let tok = XmlTokenizer::new(tb, opts.tokenizer);
    XmlParser { tokenizer: tok}
}

/// An XML parser,
/// ready to receive Unicode input through the `tendril::TendrilSink` trait’s methods.
pub struct XmlParser<Sink> where Sink: TreeSink {
    /// Tokenizer used by XmlParser.
    pub tokenizer: XmlTokenizer<XmlTreeBuilder<Sink::Handle, Sink>>,
}

impl<Sink: TreeSink> TendrilSink<tendril::fmt::UTF8> for XmlParser<Sink> {

    type Output = Sink::Output;

    fn process(&mut self, t: StrTendril) {
        self.tokenizer.feed(t)
    }

    // FIXME: Is it too noisy to report every character decoding error?
    fn error(&mut self, desc: Cow<'static, str>) {
        self.tokenizer.sink_mut().sink_mut().parse_error(desc)
    }

    fn finish(mut self) -> Self::Output {
        self.tokenizer.end();
        self.tokenizer.unwrap().unwrap().finish()
    }
}

impl<Sink: TreeSink> XmlParser<Sink> {
    /// Wrap this parser into a `TendrilSink` that accepts UTF-8 bytes.
    ///
    /// Use this when your input is bytes that are known to be in the UTF-8 encoding.
    /// Decoding is lossy, like `String::from_utf8_lossy`.
    pub fn from_utf8(self) -> Utf8LossyDecoder<Self> {
        Utf8LossyDecoder::new(self)
    }

    /// Wrap this parser into a `TendrilSink` that accepts bytes
    /// and tries to detect the correct character encoding.
    ///
    /// Currently this looks for a Byte Order Mark,
    /// then uses `BytesOpts::transport_layer_encoding`,
    /// then falls back to UTF-8.
    ///
    /// FIXME(https://github.com/servo/html5ever/issues/18): this should look for `<meta>` elements
    pub fn from_bytes(self, opts: BytesOpts) -> BytesParser<Sink> {
        BytesParser {
            state: BytesParserState::Initial { parser: self },
            opts: opts,
        }
    }
}

/// Options for choosing a character encoding
#[derive(Clone, Default)]
pub struct BytesOpts {
    /// The character encoding specified by the transport layer, if any.
    /// In HTTP for example, this is the `charset` parameter of the `Content-Type` response header.
    pub transport_layer_encoding: Option<EncodingRef>,
}

/// An HTML parser,
/// ready to receive bytes input through the `tendril::TendrilSink` trait’s methods.
///
/// See `Parser::from_bytes`.
pub struct BytesParser<Sink> where Sink: TreeSink {
    state: BytesParserState<Sink>,
    opts: BytesOpts,
}

enum BytesParserState<Sink> where Sink: TreeSink {
    Initial {
        parser: XmlParser<Sink>,
    },
    Buffering {
        parser: XmlParser<Sink>,
        buffer: ByteTendril
    },
    Parsing {
        decoder: LossyDecoder<XmlParser<Sink>>,
    },
    Transient
}

impl<Sink: TreeSink> BytesParser<Sink> {
    /// Access the underlying Parser
    pub fn str_parser(&self) -> &XmlParser<Sink> {
        match self.state {
            BytesParserState::Initial { ref parser } => parser,
            BytesParserState::Buffering { ref parser, .. } => parser,
            BytesParserState::Parsing { ref decoder } => decoder.inner_sink(),
            BytesParserState::Transient => unreachable!(),
        }
    }

    /// Access the underlying Parser
    pub fn str_parser_mut(&mut self) -> &mut XmlParser<Sink> {
        match self.state {
            BytesParserState::Initial { ref mut parser } => parser,
            BytesParserState::Buffering { ref mut parser, .. } => parser,
            BytesParserState::Parsing { ref mut decoder } => decoder.inner_sink_mut(),
            BytesParserState::Transient => unreachable!(),
        }
    }

    /// Insert a Unicode chunk in the middle of the byte stream.
    ///
    /// This is e.g. for supporting `document.write`.
    pub fn process_unicode(&mut self, t: StrTendril) {
        if t.is_empty() {
            return  // Don’t prevent buffering/encoding detection
        }
        if let BytesParserState::Parsing { ref mut decoder } = self.state {
            decoder.inner_sink_mut().process(t)
        } else {
            match mem::replace(&mut self.state, BytesParserState::Transient) {
                BytesParserState::Initial { mut parser } => {
                    parser.process(t);
                    self.start_parsing(parser, ByteTendril::new())
                }
                BytesParserState::Buffering { parser, buffer } => {
                    self.start_parsing(parser, buffer);
                    if let BytesParserState::Parsing { ref mut decoder } = self.state {
                        decoder.inner_sink_mut().process(t)
                    } else {
                        unreachable!()
                    }
                }
                BytesParserState::Parsing { .. } | BytesParserState::Transient => unreachable!(),
            }
        }
    }

    fn start_parsing(&mut self, parser: XmlParser<Sink>, buffer: ByteTendril) {
        let encoding = detect_encoding(&buffer, &self.opts);
        let mut decoder = LossyDecoder::new(encoding, parser);
        decoder.process(buffer);
        self.state = BytesParserState::Parsing { decoder: decoder }
    }
}

impl<Sink: TreeSink> TendrilSink<tendril::fmt::Bytes> for BytesParser<Sink> {
    fn process(&mut self, t: ByteTendril) {
        if let &mut BytesParserState::Parsing { ref mut decoder } = &mut self.state {
            return decoder.process(t)
        }
        let (parser, buffer) = match mem::replace(&mut self.state, BytesParserState::Transient) {
            BytesParserState::Initial{ parser } => (parser, t),
            BytesParserState::Buffering { parser, mut buffer } => {
                buffer.push_tendril(&t);
                (parser, buffer)
            }
            BytesParserState::Parsing { .. } | BytesParserState::Transient => unreachable!(),
        };
        if buffer.len32() >= PRESCAN_BYTES {
            self.start_parsing(parser, buffer)
        } else {
            self.state = BytesParserState::Buffering {
                parser: parser,
                buffer: buffer,
            }
        }
    }

    fn error(&mut self, desc: Cow<'static, str>) {
        match self.state {
            BytesParserState::Initial { ref mut parser } => parser.error(desc),
            BytesParserState::Buffering { ref mut parser, .. } => parser.error(desc),
            BytesParserState::Parsing { ref mut decoder } => decoder.error(desc),
            BytesParserState::Transient => unreachable!(),
        }
    }

    type Output = Sink::Output;

    fn finish(self) -> Self::Output {
        match self.state {
            BytesParserState::Initial { parser } => parser.finish(),
            BytesParserState::Buffering { parser, buffer } => {
                let encoding = detect_encoding(&buffer, &self.opts);
                let mut decoder = LossyDecoder::new(encoding, parser);
                decoder.process(buffer);
                decoder.finish()
            },
            BytesParserState::Parsing { decoder } => decoder.finish(),
            BytesParserState::Transient => unreachable!(),
        }
    }
}

/// How many bytes does detect_encoding() need
// FIXME(#18): should be 1024 for <meta> elements.
const PRESCAN_BYTES: u32 = 3;

/// https://html.spec.whatwg.org/multipage/syntax.html#determining-the-character-encoding
fn detect_encoding(bytes: &ByteTendril, opts: &BytesOpts) -> EncodingRef {
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        return encoding::all::UTF_8
    }
    if bytes.starts_with(b"\xFE\xFF") {
        return encoding::all::UTF_16BE
    }
    if bytes.starts_with(b"\xFF\xFE") {
        return encoding::all::UTF_16LE
    }
    if let Some(encoding) = opts.transport_layer_encoding {
        return encoding
    }
    // FIXME(#18): <meta> etc.
    return encoding::all::UTF_8
}

#[cfg(test)]
mod tests {
    use rcdom::RcDom;
    use serialize::serialize;
    use tendril::TendrilSink;
    use super::*;

    #[test]
    fn el_ns_serialize() {
        assert_eq_serialization("<a:title xmlns:a=\"http://www.foo.org/\" value=\"test\">Test</a:title>",
            parse_document(RcDom::default(), XmlParseOpts::default())
                .from_utf8()
                .one("<a:title xmlns:a=\"http://www.foo.org/\" value=\"test\">Test</title>".as_bytes()));
    }

    #[test]
    fn nested_ns_serialize() {
        assert_eq_serialization("<a:x xmlns:a=\"http://www.foo.org/\" xmlns:b=\"http://www.bar.org/\" value=\"test\"><b:y/></a:x>",
            parse_document(RcDom::default(), XmlParseOpts::default())
                .from_utf8()
                .one("<a:x xmlns:a=\"http://www.foo.org/\" xmlns:b=\"http://www.bar.org/\" value=\"test\"><b:y/></a:x>".as_bytes()));
    }

    #[test]
    fn def_ns_serialize() {
        assert_eq_serialization("<table xmlns=\"html4\"><td></td></table>",
            parse_document(RcDom::default(), XmlParseOpts::default())
                .from_utf8()
                .one("<table xmlns=\"html4\"><td></td></table>".as_bytes()));
    }

    #[test]
    fn undefine_ns_serialize() {
        assert_eq_serialization("<a:x xmlns:a=\"http://www.foo.org\"><a:y xmlns:a=\"\"><a:z/></a:y</a:x>",
            parse_document(RcDom::default(), XmlParseOpts::default())
                .from_utf8()
                .one("<a:x xmlns:a=\"http://www.foo.org\"><a:y xmlns:a=\"\"><a:z/></a:y</a:x>".as_bytes()));
    }

    #[test]
    fn redefine_default_ns_serialize() {
        assert_eq_serialization("<x xmlns=\"http://www.foo.org\"><y xmlns=\"\"><z/></y</x>",
            parse_document(RcDom::default(), XmlParseOpts::default())
                .from_utf8()
                .one("<x xmlns=\"http://www.foo.org\"><y xmlns=\"\"><z/></y</x>".as_bytes()));
    }

    #[test]
    fn attr_serialize() {
        assert_serialization("<title value=\"test\">Test</title>",
            parse_document(RcDom::default(), XmlParseOpts::default())
                .from_utf8()
                .one("<title value='test'>Test".as_bytes()));
    }

    #[test]
    fn from_utf8() {
        assert_serialization("<title>Test</title>",
            parse_document(RcDom::default(), XmlParseOpts::default())
                .from_utf8()
                .one("<title>Test".as_bytes()));
    }

    #[test]
    fn from_bytes_one() {
        assert_serialization("<title>Test</title>",
            parse_document(RcDom::default(), XmlParseOpts::default())
                .from_bytes(BytesOpts::default())
                .one("<title>Test".as_bytes()));
    }

    fn assert_eq_serialization(text: &'static str, dom: RcDom) {
        let mut serialized = Vec::new();
        serialize(&mut serialized, &dom.document, Default::default()).unwrap();

        let dom_from_text = parse_document(RcDom::default(), XmlParseOpts::default())
            .from_bytes(BytesOpts::default())
            .one(text.as_bytes());

        let mut reserialized = Vec::new();
        serialize(&mut reserialized, &dom_from_text.document, Default::default()).unwrap();

        assert_eq!(String::from_utf8(serialized).unwrap(),
                   String::from_utf8(reserialized).unwrap());
    }

    fn assert_serialization(text: &'static str, dom: RcDom) {
        let mut serialized = Vec::new();
        serialize(&mut serialized, &dom.document, Default::default()).unwrap();
        assert_eq!(String::from_utf8(serialized).unwrap(),
                   text);
    }
}
