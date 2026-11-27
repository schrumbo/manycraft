//! The server's command tree, read only to find which arguments of a typed command the game signs: the `message` arguments.

use anyhow::{Result, bail, ensure};

use super::v777::ARGUMENTS;
use super::wire::In;

const MAX_NODES: usize = 1 << 16;
const FLAGS_TYPE: u8 = 0b11;
const FLAG_REDIRECT: u8 = 0b1000;
const FLAG_SUGGESTIONS: u8 = 0b1_0000;

/// How much of the command text an argument takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reader {
    Message,
    Greedy,
    Word,
    Phrase,
    Token,
    Tokens(u8),
}

enum Kind {
    Root,
    Literal(String),
    Argument { name: String, reader: Reader },
}

struct Node {
    kind: Kind,
    children: Vec<usize>,
    redirect: Option<usize>,
}

/// A command argument the game signs, with the text it signs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Signed {
    pub name: String,
    pub value: String,
}

pub struct Tree {
    nodes: Vec<Node>,
    root: usize,
}

impl Tree {
    /// Reads the body of the server's command packet.
    pub fn read(r: &mut In) -> Result<Self> {
        let count = usize::try_from(r.varint()?)?;
        ensure!(count <= MAX_NODES, "a command tree of {count} nodes");
        let mut nodes = Vec::new();
        for _ in 0..count {
            let flags = r.u8()?;
            let mut children = Vec::new();
            for _ in 0..r.varint()? {
                children.push(usize::try_from(r.varint()?)?);
            }
            let redirect = if flags & FLAG_REDIRECT != 0 { Some(usize::try_from(r.varint()?)?) } else { None };
            let kind = match flags & FLAGS_TYPE {
                0 => Kind::Root,
                1 => Kind::Literal(r.string()?),
                2 => {
                    let name = r.string()?;
                    let reader = reader(usize::try_from(r.varint()?)?, r)?;
                    if flags & FLAG_SUGGESTIONS != 0 {
                        r.string()?;
                    }
                    Kind::Argument { name, reader }
                }
                other => bail!("command node type {other}"),
            };
            nodes.push(Node { kind, children, redirect });
        }
        let root = usize::try_from(r.varint()?)?;
        ensure!(root < nodes.len(), "no root in the command tree");
        Ok(Self { nodes, root })
    }

    /// The message arguments of `command` (without its slash) in the order the game signs them, as far as the tree parses it.
    pub fn signable(&self, command: &str) -> Vec<Signed> {
        let mut walk = Walk { tree: self, text: command, best: (0, Vec::new()) };
        walk.go(self.root, 0, &mut Vec::new());
        walk.best.1
    }

    fn after(&self, node: usize) -> &[usize] {
        let node = &self.nodes[node];
        match node.redirect.and_then(|r| self.nodes.get(r)) {
            Some(target) => &target.children,
            None => &node.children,
        }
    }
}

fn reader(id: usize, r: &mut In) -> Result<Reader> {
    let name = *ARGUMENTS.get(id).ok_or_else(|| anyhow::anyhow!("command argument type {id}"))?;
    Ok(match name {
        "minecraft:message" => Reader::Message,
        "brigadier:string" => match r.varint()? {
            0 => Reader::Word,
            1 => Reader::Phrase,
            _ => Reader::Greedy,
        },
        "brigadier:float" | "brigadier:integer" => limits(r, 4)?,
        "brigadier:double" | "brigadier:long" => limits(r, 8)?,
        "minecraft:entity" | "minecraft:score_holder" => {
            r.u8()?;
            Reader::Token
        }
        "minecraft:time" => {
            r.take(4)?;
            Reader::Token
        }
        "minecraft:resource_or_tag" | "minecraft:resource_or_tag_key" | "minecraft:resource" | "minecraft:resource_key" | "minecraft:resource_selector" => {
            r.string()?;
            Reader::Token
        }
        "minecraft:block_pos" | "minecraft:vec3" => Reader::Tokens(3),
        "minecraft:column_pos" | "minecraft:vec2" | "minecraft:rotation" => Reader::Tokens(2),
        _ => Reader::Token,
    })
}

fn limits(r: &mut In, width: usize) -> Result<Reader> {
    let flags = r.u8()?;
    r.take(width * (flags & 0b11).count_ones() as usize)?;
    Ok(Reader::Token)
}

struct Walk<'a> {
    tree: &'a Tree,
    text: &'a str,
    best: (usize, Vec<Signed>),
}

impl Walk<'_> {
    fn keep(&mut self, at: usize, signed: &[Signed]) {
        if at >= self.best.0 {
            self.best = (at, signed.to_vec());
        }
    }

    fn next(&mut self, node: usize, end: usize, signed: &mut Vec<Signed>) -> bool {
        if end >= self.text.len() {
            self.keep(end, signed);
            true
        } else if self.text.as_bytes()[end] == b' ' {
            self.go(node, end + 1, signed)
        } else {
            self.keep(end, signed);
            false
        }
    }

    fn go(&mut self, node: usize, start: usize, signed: &mut Vec<Signed>) -> bool {
        if start >= self.text.len() {
            self.keep(start, signed);
            return false;
        }
        let tree = self.tree;
        let rest = &self.text[start..];
        for &child in tree.after(node) {
            if let Some(Node { kind: Kind::Literal(name), .. }) = tree.nodes.get(child)
                && rest.starts_with(name.as_str())
                && rest[name.len()..].chars().next().is_none_or(|c| c == ' ')
                && self.next(child, start + name.len(), signed)
            {
                return true;
            }
        }
        for &child in tree.after(node) {
            let Some(Node { kind: Kind::Argument { name, reader }, .. }) = tree.nodes.get(child) else { continue };
            let Some(end) = read(self.text, start, *reader) else { continue };
            let pushed = *reader == Reader::Message;
            if pushed {
                signed.push(Signed { name: name.clone(), value: self.text[start..end].to_owned() });
            }
            if self.next(child, end, signed) {
                return true;
            }
            if pushed {
                signed.pop();
            }
        }
        self.keep(start, signed);
        false
    }
}

fn read(text: &str, start: usize, reader: Reader) -> Option<usize> {
    match reader {
        Reader::Message | Reader::Greedy => Some(text.len()),
        Reader::Word => Some(word(text, start)).filter(|end| *end > start),
        Reader::Phrase => {
            if text[start..].starts_with(['"', '\'']) { quoted(text, start) } else { Some(word(text, start)).filter(|end| *end > start) }
        }
        Reader::Token => Some(token(text, start)).filter(|end| *end > start),
        Reader::Tokens(n) => {
            let mut end = start;
            for i in 0..n {
                if i > 0 {
                    if text[end..].starts_with(' ') { end += 1 } else { return None }
                }
                let next = token(text, end);
                if next == end {
                    return None;
                }
                end = next;
            }
            Some(end)
        }
    }
}

fn word(text: &str, start: usize) -> usize {
    text[start..].find(' ').map_or(text.len(), |i| start + i)
}

fn quoted(text: &str, start: usize) -> Option<usize> {
    let mut chars = text[start..].char_indices();
    let (_, quote) = chars.next()?;
    let mut escaped = false;
    for (i, c) in chars {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            c if c == quote => return Some(start + i + 1),
            _ => {}
        }
    }
    None
}

/// A run up to the next space that is not inside brackets or quotes, which is how selectors, item stacks and NBT stay whole.
fn token(text: &str, start: usize) -> usize {
    let mut depth = 0usize;
    let mut chars = text[start..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            ' ' if depth == 0 => return start + i,
            '"' | '\'' => match quoted(text, start + i) {
                Some(end) => while chars.next_if(|(j, _)| start + j < end).is_some() {},
                None => return text.len(),
            },
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    const VANILLA: &str = "1a000a0102030405060708090a01010b026d6505010c0468656c7005010d046c69737401010e036d73670900040474656c6c090004017701020f100672616e646f6d010111077465616d6d736709000802746d0101120774726967676572060006616374696f6e14060007636f6d6d616e6405020500057575696473020113077461726765747306020101140576616c756501011504726f6c6c0600076d6573736167651416021617096f626a65637469766518146d696e6563726166743a61736b5f7365727665720600076d6573736167651406000572616e67652706000572616e676527010118036164640101190373657406000576616c7565030006000576616c7565030000";

    fn vanilla() -> Tree {
        let body = hex(VANILLA);
        let mut r = In::new(&body);
        let tree = Tree::read(&mut r).unwrap();
        assert!(r.take(1).is_err(), "the whole packet was read");
        tree
    }

    fn signed(tree: &Tree, command: &str) -> Vec<(String, String)> {
        tree.signable(command).into_iter().map(|s| (s.name, s.value)).collect()
    }

    fn pair(name: &str, value: &str) -> Vec<(String, String)> {
        vec![(name.to_owned(), value.to_owned())]
    }

    #[test]
    fn the_message_of_msg_and_its_aliases_is_signed_and_nothing_else() {
        let tree = vanilla();
        assert_eq!(tree.nodes.len(), 26);
        assert_eq!(signed(&tree, "msg Steve hello there"), pair("message", "hello there"));
        assert_eq!(signed(&tree, "tell Steve hi"), pair("message", "hi"));
        assert_eq!(signed(&tree, "w Steve  two  spaces"), pair("message", " two  spaces"));
        assert_eq!(signed(&tree, "msg @a[name=\"a b\"] hi there"), pair("message", "hi there"));
        assert_eq!(signed(&tree, "teammsg all of you"), pair("message", "all of you"));
        for command in ["list", "help", "random value 1..6", "trigger x set 3", "msg", "msg Steve", "nonsense 1 2"] {
            assert!(signed(&tree, command).is_empty(), "{command}");
        }
    }

    fn node(flags: u8, children: &[u8], redirect: Option<u8>, name: &str, parser: &[u8]) -> Vec<u8> {
        let mut out = vec![flags, children.len() as u8];
        out.extend_from_slice(children);
        out.extend(redirect);
        if flags & 3 != 0 {
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
        }
        out.extend_from_slice(parser);
        out
    }

    #[test]
    fn a_redirect_back_to_the_root_signs_the_command_it_runs() {
        let message = ARGUMENTS.iter().position(|a| *a == "minecraft:message").unwrap() as u8;
        let mut body = vec![5];
        body.extend(node(0, &[1, 3], None, "", &[]));
        body.extend(node(1, &[2], None, "execute", &[]));
        body.extend(node(9, &[], Some(0), "run", &[]));
        body.extend(node(1, &[4], None, "say", &[]));
        body.extend(node(6, &[], None, "message", &[message]));
        body.push(0);
        let tree = Tree::read(&mut In::new(&body)).unwrap();
        assert_eq!(signed(&tree, "execute run say hello"), pair("message", "hello"));
        assert_eq!(signed(&tree, "execute run execute run say a b"), pair("message", "a b"));
        assert_eq!(signed(&tree, "execute run"), Vec::<(String, String)>::new());
    }

    #[test]
    fn a_tree_that_is_cut_short_or_absurd_is_refused() {
        let body = hex(VANILLA);
        for cut in [0, 1, 5, 40, body.len() - 1] {
            assert!(Tree::read(&mut In::new(&body[..cut])).is_err(), "cut at {cut}");
        }
        assert!(Tree::read(&mut In::new(&[0xff, 0xff, 0xff, 0x7f])).is_err());
    }

    #[test]
    fn tokens_keep_brackets_and_quotes_whole() {
        assert_eq!(token("@e[name=\"a b\",tag=x] rest", 0), 20);
        assert_eq!(token("stone[a=1] x", 0), 10);
        assert_eq!(token("'it\\'s' x", 0), 7);
        assert_eq!(quoted("\"a\\\"b\" c", 0), Some(6));
    }
}