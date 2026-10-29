//! Текстовый VDF (KeyValues Valve): то, что печатают `steamcmd +app_info_print` и Steam в своих .vdf.
//!
//! Ключ — строка, значение — строка или вложенный объект в `{}`. Строки в кавычках с `\\ \" \n \t`
//! или без кавычек; `//` — комментарий до конца строки; условия вида `[$WIN32]` пропускаются.
//! Порядок и повторы ключей сохраняются: Valve их допускает.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Obj(Obj),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Obj(_) => None,
        }
    }

    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Value::Obj(o) => Some(o),
            Value::Str(_) => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Obj(pub Vec<(String, Value)>);

impl Obj {
    /// Первое значение по ключу. Ключи без учёта регистра ASCII — как в KeyValues у Valve.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    pub fn obj(&self, key: &str) -> Option<&Obj> {
        self.get(key).and_then(Value::as_obj)
    }

    /// Вложенный объект по цепочке ключей: `obj.path(&["ufs", "savefiles"])`.
    pub fn path(&self, keys: &[&str]) -> Option<&Obj> {
        keys.iter().try_fold(self, |o, k| o.obj(k))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("VDF, строка {line}: {msg}")]
pub struct VdfError {
    pub line: usize,
    pub msg: &'static str,
}

/// Весь документ: пары ключ-значение до конца текста.
pub fn parse(text: &str) -> Result<Obj, VdfError> {
    let mut p = Parser::new(text);
    let obj = p.pairs(false)?;
    Ok(obj)
}

/// Одна пара `"ключ" { … }` с начала текста; хвост после неё не читается (у steamcmd там служебные
/// строки). Возвращает ключ, значение и сколько байт заняла пара.
pub fn parse_pair(text: &str) -> Result<(String, Value, usize), VdfError> {
    let mut p = Parser::new(text);
    let key = match p.token()? {
        Tok::Str(k) => k,
        Tok::Eof => return Err(p.err("пусто")),
        _ => return Err(p.err("ждал ключ")),
    };
    let value = p.value()?;
    Ok((key, value, p.pos))
}

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String),
    Open,
    Close,
    Eof,
}

struct Parser<'a> {
    s: &'a str,
    pos: usize,
    line: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser { s, pos: 0, line: 1 }
    }

    fn err(&self, msg: &'static str) -> VdfError {
        VdfError { line: self.line, msg }
    }

    fn peek(&self) -> Option<char> {
        self.s[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn skip_space(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.s[self.pos..].starts_with("//") => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                _ => return,
            }
        }
    }

    fn token(&mut self) -> Result<Tok, VdfError> {
        loop {
            self.skip_space();
            match self.peek() {
                None => return Ok(Tok::Eof),
                Some('{') => {
                    self.bump();
                    return Ok(Tok::Open);
                }
                Some('}') => {
                    self.bump();
                    return Ok(Tok::Close);
                }
                Some('"') => {
                    self.bump();
                    return self.quoted().map(Tok::Str);
                }
                // Условие платформы после значения или ключа: в app_info_print его нет, в .vdf бывает.
                Some('[') => {
                    while self.peek().is_some_and(|c| c != ']' && c != '\n') {
                        self.bump();
                    }
                    if self.bump() != Some(']') {
                        return Err(self.err("незакрытое условие [ ]"));
                    }
                }
                Some(_) => {
                    let start = self.pos;
                    while self.peek().is_some_and(|c| !c.is_whitespace() && !matches!(c, '{' | '}' | '"')) {
                        self.bump();
                    }
                    return Ok(Tok::Str(self.s[start..self.pos].to_string()));
                }
            }
        }
    }

    fn quoted(&mut self) -> Result<String, VdfError> {
        let start_line = self.line;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(VdfError { line: start_line, msg: "незакрытая кавычка" }),
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('\\') => out.push('\\'),
                    Some('"') => out.push('"'),
                    // Неизвестную последовательность Valve оставляет как есть.
                    Some(c) => {
                        out.push('\\');
                        out.push(c);
                    }
                    None => return Err(VdfError { line: start_line, msg: "незакрытая кавычка" }),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn value(&mut self) -> Result<Value, VdfError> {
        match self.token()? {
            Tok::Str(s) => Ok(Value::Str(s)),
            Tok::Open => self.pairs(true).map(Value::Obj),
            Tok::Close => Err(self.err("ждал значение, а тут }")),
            Tok::Eof => Err(self.err("ждал значение, а текст кончился")),
        }
    }

    /// Пары до `}` (внутри объекта) или до конца текста (документ целиком).
    fn pairs(&mut self, nested: bool) -> Result<Obj, VdfError> {
        let mut obj = Obj::default();
        loop {
            match self.token()? {
                Tok::Str(k) => {
                    let v = self.value()?;
                    obj.0.push((k, v));
                }
                Tok::Close if nested => return Ok(obj),
                Tok::Eof if !nested => return Ok(obj),
                Tok::Close => return Err(self.err("лишняя }")),
                Tok::Eof => return Err(self.err("незакрытая {")),
                Tok::Open => return Err(self.err("{ без ключа")),
            }
        }
    }
}

impl fmt::Display for Obj {
    /// Обратно в VDF с табами, как печатает Steam.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn quote(s: &str) -> String {
            let mut q = String::with_capacity(s.len() + 2);
            q.push('"');
            for c in s.chars() {
                match c {
                    '"' => q.push_str("\\\""),
                    '\\' => q.push_str("\\\\"),
                    '\n' => q.push_str("\\n"),
                    '\t' => q.push_str("\\t"),
                    c => q.push(c),
                }
            }
            q.push('"');
            q
        }
        fn write(o: &Obj, depth: usize, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let pad = "\t".repeat(depth);
            for (k, v) in &o.0 {
                match v {
                    Value::Str(s) => writeln!(f, "{pad}{}\t\t{}", quote(k), quote(s))?,
                    Value::Obj(inner) => {
                        writeln!(f, "{pad}{}\n{pad}{{", quote(k))?;
                        write(inner, depth + 1, f)?;
                        writeln!(f, "{pad}}}")?;
                    }
                }
            }
            Ok(())
        }
        write(self, 0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_order() {
        let o = parse("\"a\" { \"x\" \"1\" \"y\" { \"z\" \"2\" } \"x\" \"3\" }").unwrap();
        let a = o.obj("a").unwrap();
        assert_eq!(a.str("x"), Some("1"));
        assert_eq!(a.path(&["y"]).unwrap().str("z"), Some("2"));
        assert_eq!(a.0.len(), 3);
        assert_eq!(a.0[2].1, Value::Str("3".into()));
    }

    #[test]
    fn case_insensitive_keys() {
        let o = parse("\"UFS\" { \"SaveFiles\" { } }").unwrap();
        assert!(o.path(&["ufs", "savefiles"]).is_some());
    }

    #[test]
    fn escapes() {
        let o = parse(r#""k" "x64\\Hades.exe" "w" "x64\\" "q" "a \"b\" \n" "u" "\d""#).unwrap();
        assert_eq!(o.str("k"), Some(r"x64\Hades.exe"));
        // `\\"` — экранированный слэш и конец строки, а не экранированная кавычка.
        assert_eq!(o.str("w"), Some(r"x64\"));
        assert_eq!(o.str("q"), Some("a \"b\" \n"));
        assert_eq!(o.str("u"), Some(r"\d"));
    }

    #[test]
    fn unquoted_comments_conditions() {
        let text = "// шапка\nroot\n{\n  key value // хвост\n  \"os\" \"mac\" [$OSX]\n  braces{ \"in\" \"1\" }\n}\n";
        let o = parse(text).unwrap();
        let r = o.obj("root").unwrap();
        assert_eq!(r.str("key"), Some("value"));
        assert_eq!(r.str("os"), Some("mac"));
        assert_eq!(r.path(&["braces"]).unwrap().str("in"), Some("1"));
    }

    #[test]
    fn braces_inside_strings() {
        let o = parse("\"a\" { \"t\" \"{ не объект }\" }").unwrap();
        assert_eq!(o.obj("a").unwrap().str("t"), Some("{ не объект }"));
    }

    #[test]
    fn errors_have_lines() {
        assert_eq!(parse("\"a\"\n{\n\"b\" \"c\"\n").unwrap_err(), VdfError { line: 4, msg: "незакрытая {" });
        assert_eq!(parse("\"a\" \"b\" }").unwrap_err().msg, "лишняя }");
        assert_eq!(parse("\"a\"\n\"b").unwrap_err(), VdfError { line: 2, msg: "незакрытая кавычка" });
        assert_eq!(parse("\"a\"").unwrap_err().msg, "ждал значение, а текст кончился");
    }

    #[test]
    fn pair_stops_after_object() {
        let text = "\"1\"\n{\n\t\"a\"\t\t\"b\"\n}\nUnloading Steam API...OK\n";
        let (k, v, n) = parse_pair(text).unwrap();
        assert_eq!(k, "1");
        assert_eq!(v.as_obj().unwrap().str("a"), Some("b"));
        assert!(text[n..].trim_start().starts_with("Unloading"));
    }

    #[test]
    fn display_roundtrip() {
        let o = parse("\"a\" { \"p\" \"x\\\\y \\\"z\\\"\" \"n\" { } }").unwrap();
        assert_eq!(parse(&o.to_string()).unwrap(), o);
    }
}
