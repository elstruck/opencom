//! `{placeholder}` expansion for automation text, URLs, bodies and OSC arguments.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Vars(HashMap<String, String>);

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(mut self, k: &str, v: impl Into<String>) -> Self {
        self.0.insert(k.to_string(), v.into());
        self
    }

    pub fn insert(&mut self, k: &str, v: impl Into<String>) {
        self.0.insert(k.to_string(), v.into());
    }

    /// Replace `{name}` with its value. Unknown names stay as they are. `{{` and `}}` escape braces.
    pub fn expand(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '{' {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let mut name = String::new();
                let mut closed = false;
                for n in chars.by_ref() {
                    if n == '}' {
                        closed = true;
                        break;
                    }
                    name.push(n);
                }
                match (closed, self.0.get(name.trim())) {
                    (true, Some(v)) => out.push_str(v),
                    (true, None) => {
                        out.push('{');
                        out.push_str(&name);
                        out.push('}');
                    }
                    (false, _) => {
                        out.push('{');
                        out.push_str(&name);
                    }
                }
            } else if c == '}' && chars.peek() == Some(&'}') {
                chars.next();
                out.push('}');
            } else {
                out.push(c);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands() {
        let v = Vars::new().set("text", "go now").set("channel", "SM");
        assert_eq!(v.expand("[{channel}] {text}"), "[SM] go now");
        assert_eq!(v.expand("{unknown} {{x}}"), "{unknown} {x}");
        assert_eq!(v.expand("open {brace"), "open {brace");
    }
}
