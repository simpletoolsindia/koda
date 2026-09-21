//! Syntax-aware extraction with Tree-sitter, for the languages it covers.
//!
//! The lexical extractor in `graph` reads a line at a time with patterns. That
//! is fast and language-agnostic, and it is wrong in the ways line patterns are
//! wrong: `def fake():` inside a docstring is a definition to it, a Go method's
//! receiver is invisible, a TypeScript method is not a `function`, and it has
//! no idea where anything *ends* — so the search index assumed every
//! definition ran until the next one began.
//!
//! This module parses Rust, Python, JavaScript, TypeScript and Go properly and
//! returns the same facts `graph::parse_file` always did — definitions,
//! imports, identifiers — in the same conventions (a method is recorded both
//! bare and as `Type::name`), plus what only a parser knows: each definition's
//! real extent and the documentation attached above it.
//!
//! It is syntax, not semantics. It knows `foo()` is a call to something named
//! `foo`; it does not know which `foo`. That is the language server's job
//! (`lsp`), and the graph labels the two kinds of answer differently.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use tree_sitter::{Language, Node, Parser};

/// One definition, with where it really starts and ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynDef {
    pub name: String,
    pub kind: &'static str,
    /// 1-based line of the definition itself.
    pub line: usize,
    /// 1-based first line of the comments, attributes or decorators attached
    /// above it — where a search chunk for it should begin.
    pub doc: usize,
    /// 1-based last line.
    pub end: usize,
    /// Nested inside another definition (a method, a closure-held fn).
    pub nested: bool,
}

#[derive(Debug, Default)]
pub struct SynParse {
    pub defs: Vec<SynDef>,
    pub imports: Vec<String>,
    pub ids: BTreeSet<String>,
}

/// Whether this build and this language get a real parse.
pub fn covers(lang: &str) -> bool {
    matches!(lang, "rust" | "python" | "javascript" | "typescript" | "go")
}

fn language(lang: &str, path: &str) -> Option<Language> {
    Some(match lang {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" if path.ends_with(".tsx") => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        _ => return None,
    })
}

thread_local! {
    /// One parser per language per thread: building one costs more than
    /// parsing a small file, and the scan parses on several threads at once.
    static PARSERS: RefCell<HashMap<(&'static str, bool), Parser>> = RefCell::new(HashMap::new());
}

/// Parse one file. `None` when the language is not covered or the parser
/// gives up, and the caller falls back to the lexical extractor.
pub fn parse(lang: &'static str, path: &str, text: &str, keywords: &[&str]) -> Option<SynParse> {
    if !covers(lang) {
        return None;
    }
    let tsx = path.ends_with(".tsx");
    let tree = PARSERS.with(|cell| {
        let mut parsers = cell.borrow_mut();
        let parser = match parsers.entry((lang, tsx)) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let mut p = Parser::new();
                p.set_language(&language(lang, path)?).ok()?;
                v.insert(p)
            }
        };
        parser.parse(text, None)
    })?;
    let src = text.as_bytes();
    let mut out = SynParse::default();
    let mut w = Walk {
        lang,
        src,
        keywords,
        out: &mut out,
    };
    w.node(tree.root_node(), &Scope::default());
    Some(out)
}

/// Where a node sits: the type whose members it defines, and whether it is
/// already inside a definition.
#[derive(Clone, Default)]
struct Scope {
    owner: Option<String>,
    in_def: bool,
}

struct Walk<'a> {
    lang: &'static str,
    src: &'a [u8],
    keywords: &'a [&'a str],
    out: &'a mut SynParse,
}

impl Walk<'_> {
    fn text(&self, n: Node) -> String {
        n.utf8_text(self.src).unwrap_or("").to_string()
    }

    fn field(&self, n: Node, f: &str) -> Option<String> {
        n.child_by_field_name(f).map(|c| self.text(c))
    }

    /// The first line of the comments/attributes/decorators directly above `n`
    /// (touching, no blank line between), or `n`'s own first line.
    fn doc_start(&self, n: Node) -> usize {
        let mut start = n.start_position().row;
        let mut cur = n;
        // A decorated Python definition: the decorators are its parent.
        if let Some(p) = n.parent() {
            if p.kind() == "decorated_definition" || p.kind() == "export_statement" {
                start = p.start_position().row;
                cur = p;
            }
        }
        while let Some(prev) = cur.prev_sibling() {
            let doc_like = matches!(
                prev.kind(),
                "line_comment" | "block_comment" | "comment" | "attribute_item" | "decorator"
            );
            if !doc_like || prev.end_position().row + 1 < start {
                break;
            }
            start = prev.start_position().row;
            cur = prev;
        }
        start + 1
    }

    fn def(&mut self, n: Node, name: String, kind: &'static str, scope: &Scope) {
        if name.is_empty() {
            return;
        }
        let line = n.start_position().row + 1;
        let end = n.end_position().row + 1;
        let doc = self.doc_start(n);
        let nested = scope.in_def;
        if kind == "fn" {
            if let Some(t) = &scope.owner {
                self.out.defs.push(SynDef {
                    name: format!("{t}::{name}"),
                    kind: "method",
                    line,
                    doc,
                    end,
                    nested,
                });
            }
        }
        self.out.defs.push(SynDef {
            name,
            kind,
            line,
            doc,
            end,
            nested,
        });
    }

    fn id(&mut self, n: Node) {
        let t = self.text(n);
        if t.len() > 2
            && !t.chars().all(|c| c.is_ascii_digit())
            && !self.keywords.contains(&t.as_str())
        {
            self.out.ids.insert(t);
        }
    }

    /// A type's plain name: `Foo<T>` → `Foo`, `&mut Foo` → `Foo`, `*Foo` → `Foo`.
    fn type_name(&self, n: Node) -> String {
        let t = self.text(n);
        let t = t
            .trim_start_matches(['&', '*'])
            .trim_start_matches("mut ")
            .trim();
        let t = t.split('<').next().unwrap_or(t);
        t.rsplit("::").next().unwrap_or(t).trim().to_string()
    }

    fn children(&mut self, n: Node, scope: &Scope) {
        let mut c = n.walk();
        let kids: Vec<Node> = n.children(&mut c).collect();
        for k in kids {
            self.node(k, scope);
        }
    }

    fn node(&mut self, n: Node, scope: &Scope) {
        let kind = n.kind();
        // Leaves: identifiers only. Comments and string contents never are.
        if n.child_count() == 0 {
            if matches!(
                kind,
                "identifier"
                    | "type_identifier"
                    | "field_identifier"
                    | "property_identifier"
                    | "shorthand_property_identifier"
                    | "shorthand_field_identifier"
                    | "package_identifier"
            ) {
                self.id(n);
            }
            return;
        }
        match self.lang {
            "rust" => self.rust(n, kind, scope),
            "python" => self.python(n, kind, scope),
            "javascript" | "typescript" => self.js(n, kind, scope),
            "go" => self.go(n, kind, scope),
            _ => self.children(n, scope),
        }
    }

    fn nested_scope(&self, scope: &Scope, owner: Option<String>) -> Scope {
        Scope {
            owner: owner.or_else(|| scope.owner.clone()),
            in_def: true,
        }
    }

    fn rust(&mut self, n: Node, kind: &str, scope: &Scope) {
        let named = |k: &'static str| Some(k);
        let def_kind = match kind {
            "function_item" | "function_signature_item" => named("fn"),
            "struct_item" => named("struct"),
            "enum_item" => named("enum"),
            "union_item" => named("struct"),
            "trait_item" => named("trait"),
            "type_item" => named("type"),
            "const_item" => named("const"),
            "static_item" => named("static"),
            "mod_item" => named("mod"),
            "macro_definition" => named("macro"),
            _ => None,
        };
        match (kind, def_kind) {
            ("use_declaration", _) => {
                if let Some(a) = self.field(n, "argument") {
                    self.out.imports.push(a);
                }
                self.children(n, scope);
            }
            ("impl_item", _) => {
                let owner = n.child_by_field_name("type").map(|t| self.type_name(t));
                let inner = Scope {
                    owner,
                    in_def: scope.in_def,
                };
                self.children(n, &inner);
            }
            (_, Some(k)) => {
                let name = self.field(n, "name").unwrap_or_default();
                self.def(n, name.clone(), k, scope);
                // A trait's items belong to it; nothing else sets an owner.
                let owner = (k == "trait").then_some(name);
                let inner = if k == "trait" {
                    Scope {
                        owner,
                        in_def: scope.in_def,
                    }
                } else {
                    self.nested_scope(scope, None)
                };
                self.children(n, &inner);
            }
            _ => self.children(n, scope),
        }
    }

    fn python(&mut self, n: Node, kind: &str, scope: &Scope) {
        match kind {
            "function_definition" => {
                let name = self.field(n, "name").unwrap_or_default();
                self.def(n, name, "fn", scope);
                // A nested function belongs to no class.
                let inner = Scope {
                    owner: None,
                    in_def: true,
                };
                self.children(n, &inner);
            }
            "class_definition" => {
                let name = self.field(n, "name").unwrap_or_default();
                self.def(n, name.clone(), "class", scope);
                let inner = Scope {
                    owner: Some(name),
                    in_def: scope.in_def,
                };
                self.children(n, &inner);
            }
            "import_statement" => {
                if let Some(first) = n.named_child(0) {
                    let t = self.text(first);
                    // `import a as b` → `a`, as the lexical extractor records it.
                    let t = t.split_whitespace().next().unwrap_or("").to_string();
                    self.out.imports.push(t);
                }
                self.children(n, scope);
            }
            "import_from_statement" => {
                if let Some(m) = self.field(n, "module_name") {
                    self.out.imports.push(m);
                }
                self.children(n, scope);
            }
            _ => self.children(n, scope),
        }
    }

    fn js(&mut self, n: Node, kind: &str, scope: &Scope) {
        match kind {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                let name = self.field(n, "name").unwrap_or_default();
                self.def(n, name, "fn", scope);
                let inner = self.nested_scope(scope, None);
                self.children(
                    n,
                    &Scope {
                        owner: None,
                        ..inner
                    },
                );
            }
            "class_declaration" | "abstract_class_declaration" | "class" => {
                let name = self.field(n, "name").unwrap_or_default();
                if !name.is_empty() {
                    self.def(n, name.clone(), "class", scope);
                }
                let inner = Scope {
                    owner: (!name.is_empty()).then_some(name),
                    in_def: scope.in_def,
                };
                self.children(n, &inner);
            }
            "method_definition" | "method_signature" | "abstract_method_signature" => {
                let name = self.field(n, "name").unwrap_or_default();
                let name = name.trim_start_matches('#').to_string();
                if name != "constructor" {
                    self.def(n, name, "fn", scope);
                }
                let inner = Scope {
                    owner: None,
                    in_def: true,
                };
                self.children(n, &inner);
            }
            "interface_declaration" => {
                let name = self.field(n, "name").unwrap_or_default();
                self.def(n, name, "interface", scope);
                self.children(n, &self.nested_scope(scope, None));
            }
            "type_alias_declaration" => {
                let name = self.field(n, "name").unwrap_or_default();
                self.def(n, name, "type", scope);
                self.children(n, &self.nested_scope(scope, None));
            }
            "enum_declaration" => {
                let name = self.field(n, "name").unwrap_or_default();
                self.def(n, name, "enum", scope);
                self.children(n, &self.nested_scope(scope, None));
            }
            "lexical_declaration" | "variable_declaration" if !scope.in_def => {
                // `const f = () => …` is a function; `const LIMIT = 5` is a const.
                let is_const = self.text(n).trim_start().starts_with("const");
                let mut c = n.walk();
                let decls: Vec<Node> = n.named_children(&mut c).collect();
                for d in decls {
                    if d.kind() != "variable_declarator" {
                        continue;
                    }
                    let Some(name_node) = d.child_by_field_name("name") else {
                        continue;
                    };
                    if name_node.kind() != "identifier" {
                        continue;
                    }
                    let value = d
                        .child_by_field_name("value")
                        .map(|v| v.kind())
                        .unwrap_or("");
                    let k = match value {
                        "arrow_function" | "function_expression" | "function" => Some("fn"),
                        _ if is_const => Some("const"),
                        _ => None,
                    };
                    if let Some(k) = k {
                        let name = self.text(name_node);
                        self.def(n, name, k, scope);
                    }
                }
                self.children(n, &self.nested_scope(scope, None));
            }
            "import_statement" => {
                if let Some(s) = n.child_by_field_name("source") {
                    self.out
                        .imports
                        .push(self.text(s).trim_matches(['"', '\'', '`']).to_string());
                }
                self.children(n, scope);
            }
            "call_expression" => {
                // `require("x")`, the CommonJS import.
                let callee = n.child_by_field_name("function").map(|f| self.text(f));
                if callee.as_deref() == Some("require") {
                    if let Some(arg) = n
                        .child_by_field_name("arguments")
                        .and_then(|a| a.named_child(0))
                    {
                        if arg.kind() == "string" {
                            self.out
                                .imports
                                .push(self.text(arg).trim_matches(['"', '\'', '`']).to_string());
                        }
                    }
                }
                self.children(n, scope);
            }
            _ => self.children(n, scope),
        }
    }

    fn go(&mut self, n: Node, kind: &str, scope: &Scope) {
        match kind {
            "function_declaration" => {
                let name = self.field(n, "name").unwrap_or_default();
                self.def(n, name, "fn", scope);
                self.children(n, &self.nested_scope(scope, None));
            }
            "method_declaration" => {
                // `func (s *Server) Start()` — the receiver's type owns it.
                let owner = n
                    .child_by_field_name("receiver")
                    .and_then(|r| r.named_child(0))
                    .and_then(|p| p.child_by_field_name("type"))
                    .map(|t| self.type_name(t));
                let name = self.field(n, "name").unwrap_or_default();
                let with_owner = Scope {
                    owner,
                    in_def: scope.in_def,
                };
                self.def(n, name, "fn", &with_owner);
                self.children(n, &self.nested_scope(scope, None));
            }
            "type_spec" => {
                let name = self.field(n, "name").unwrap_or_default();
                let k = match n.child_by_field_name("type").map(|t| t.kind()) {
                    Some("struct_type") => "struct",
                    Some("interface_type") => "interface",
                    _ => "type",
                };
                self.def(n, name, k, scope);
                self.children(n, &self.nested_scope(scope, None));
            }
            "const_spec" if !scope.in_def => {
                let mut c = n.walk();
                let names: Vec<Node> = n.children_by_field_name("name", &mut c).collect();
                for nm in names {
                    let name = self.text(nm);
                    self.def(n, name, "const", scope);
                }
                self.children(n, scope);
            }
            "import_spec" => {
                if let Some(p) = self.field(n, "path") {
                    self.out.imports.push(p.trim_matches('"').to_string());
                }
                self.children(n, scope);
            }
            _ => self.children(n, scope),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(lang: &'static str, path: &str, src: &str) -> Vec<(String, &'static str)> {
        parse(lang, path, src, &[])
            .unwrap()
            .defs
            .into_iter()
            .map(|d| (d.name, d.kind))
            .collect()
    }

    fn has(v: &[(String, &'static str)], name: &str, kind: &str) -> bool {
        v.iter().any(|(n, k)| n == name && *k == kind)
    }

    /// A docstring or comment that looks like code is not code.
    #[test]
    fn comments_and_docstrings_define_nothing() {
        let py = "def real():\n    \"\"\"\n    def fake():\n        pass\n    \"\"\"\n    return 1\n# def also_fake(): pass\n";
        let d = names("python", "a.py", py);
        assert!(has(&d, "real", "fn"), "{d:?}");
        assert!(!d.iter().any(|(n, _)| n.contains("fake")), "{d:?}");

        let rs = "// fn fake() {}\n/* struct Fake; */\nfn real() {}\n";
        let d = names("rust", "a.rs", rs);
        assert_eq!(d, vec![("real".to_string(), "fn")]);
    }

    #[test]
    fn go_receiver_methods_belong_to_their_type() {
        let go = "package p\n\ntype Server struct{ n int }\n\nfunc (s *Server) Start() error {\n\treturn nil\n}\n\nfunc (s Server) Name() string { return \"\" }\n";
        let d = names("go", "s.go", go);
        assert!(has(&d, "Server", "struct"), "{d:?}");
        assert!(has(&d, "Server::Start", "method"), "{d:?}");
        assert!(has(&d, "Server::Name", "method"), "{d:?}");
        assert!(has(&d, "Start", "fn"), "{d:?}");
    }

    #[test]
    fn typescript_methods_belong_to_their_class() {
        let ts = "export class Cart {\n  private items: number[] = [];\n  total(): number {\n    return 0;\n  }\n  async save() {}\n}\nexport const add = (a: number) => a + 1;\nexport const LIMIT = 5;\ninterface Item { id: string }\n";
        let d = names("typescript", "c.ts", ts);
        assert!(has(&d, "Cart", "class"), "{d:?}");
        assert!(has(&d, "Cart::total", "method"), "{d:?}");
        assert!(has(&d, "Cart::save", "method"), "{d:?}");
        assert!(has(&d, "add", "fn"), "{d:?}");
        assert!(has(&d, "LIMIT", "const"), "{d:?}");
        assert!(has(&d, "Item", "interface"), "{d:?}");
    }

    /// A definition ends where it ends — not where the next one starts — and
    /// its doc comment is part of it.
    #[test]
    fn ranges_are_real() {
        let rs = "/// Adds.\n/// Twice.\n#[inline]\npub fn add(\n    a: u32,\n    b: u32,\n) -> u32 {\n    a + b\n}\n\nconst X: u32 = 1;\n\nimpl Foo {\n    fn inner(&self) {\n        let _ = 1;\n    }\n}\n";
        let p = parse("rust", "a.rs", rs, &[]).unwrap();
        let add = p.defs.iter().find(|d| d.name == "add").unwrap();
        assert_eq!((add.doc, add.line, add.end), (1, 4, 9), "{add:?}");
        let inner = p.defs.iter().find(|d| d.name == "Foo::inner").unwrap();
        assert_eq!((inner.line, inner.end), (14, 16));
        assert!(p.imports.is_empty());

        let py = "class A:\n    def m(self):\n        def helper():\n            pass\n        return helper\n\nx = 1\n";
        let p = parse("python", "a.py", py, &[]).unwrap();
        let m = p.defs.iter().find(|d| d.name == "m").unwrap();
        assert_eq!((m.line, m.end), (2, 5));
        let helper = p.defs.iter().find(|d| d.name == "helper").unwrap();
        assert!(helper.nested, "a function inside a function is nested");
        assert!(
            !p.defs.iter().any(|d| d.name == "A::helper"),
            "nested fns belong to no class"
        );
    }

    #[test]
    fn imports_match_the_lexical_extractors_format() {
        let p = parse(
            "rust",
            "a.rs",
            "use std::fmt::Write as _;\nuse crate::x;\n",
            &[],
        )
        .unwrap();
        assert_eq!(p.imports, vec!["std::fmt::Write as _", "crate::x"]);
        let p = parse(
            "python",
            "a.py",
            "import os.path as p\nfrom a.b import c\n",
            &[],
        )
        .unwrap();
        assert_eq!(p.imports, vec!["os.path", "a.b"]);
        let p = parse(
            "javascript",
            "a.js",
            "import x from './x';\nconst y = require(\"y\");\n",
            &[],
        )
        .unwrap();
        assert_eq!(p.imports, vec!["./x", "y"]);
        let p = parse(
            "go",
            "a.go",
            "package p\nimport (\n\t\"fmt\"\n\tio \"io\"\n)\n",
            &[],
        )
        .unwrap();
        assert_eq!(p.imports, vec!["fmt", "io"]);
    }

    /// Identifiers come from code, never from strings or comments.
    #[test]
    fn identifiers_skip_strings_and_comments() {
        let p = parse(
            "rust",
            "a.rs",
            "fn f() { let total = compute(\"not_this\"); } // nor_this\n",
            &[],
        )
        .unwrap();
        assert!(p.ids.contains("total") && p.ids.contains("compute"));
        assert!(!p.ids.contains("not_this") && !p.ids.contains("nor_this"));
    }

    #[test]
    fn uncovered_languages_fall_back() {
        assert!(parse("java", "A.java", "class A {}", &[]).is_none());
    }
    #[test]
    fn rust_items_of_every_kind() {
        let rs = "pub struct Point { x: i32 }\nenum Shape { A, B }\ntrait Draw {\n    fn draw(&self);\n}\ntype Id = u64;\nconst MAX: usize = 3;\nstatic NAME: &str = \"k\";\nmod inner {\n    pub fn helper() {}\n}\nmacro_rules! square { ($x:expr) => { $x * $x }; }\nimpl<T> Stack<T> {\n    pub fn push(&mut self, _t: T) {}\n}\n";
        let d = names("rust", "a.rs", rs);
        for (name, kind) in [
            ("Point", "struct"),
            ("Shape", "enum"),
            ("Draw", "trait"),
            ("Id", "type"),
            ("MAX", "const"),
            ("NAME", "static"),
            ("inner", "mod"),
            ("helper", "fn"),
            ("square", "macro"),
            ("Stack::push", "method"),
            ("Draw::draw", "method"),
        ] {
            assert!(has(&d, name, kind), "{name} ({kind}) missing from {d:?}");
        }
    }

    #[test]
    fn javascript_functions_classes_and_what_is_not_a_definition() {
        let js = "function plain() {}\nfunction* gen() {}\nconst arrow = () => 1;\nconst fnExpr = function () {};\nlet notConst = 5;\nclass Widget {\n  constructor() {}\n  #secret() {}\n  render() {\n    const local = () => 2;\n  }\n}\n";
        let d = names("javascript", "w.js", js);
        for (name, kind) in [
            ("plain", "fn"),
            ("gen", "fn"),
            ("arrow", "fn"),
            ("fnExpr", "fn"),
            ("Widget", "class"),
            ("Widget::render", "method"),
            ("Widget::secret", "method"),
        ] {
            assert!(has(&d, name, kind), "{name} ({kind}) missing from {d:?}");
        }
        let all: Vec<&str> = d.iter().map(|(n, _)| n.as_str()).collect();
        assert!(!all.iter().any(|n| n.ends_with("constructor")), "{all:?}");
        assert!(
            !all.contains(&"notConst"),
            "a plain `let` is not a definition"
        );
        assert!(
            !all.contains(&"local"),
            "a local inside a method is not top-level"
        );
    }

    #[test]
    fn tsx_parses_with_jsx_in_it() {
        let tsx = "export function App(): JSX.Element {\n  return <div className=\"x\">{items.map(i => <Row key={i} />)}</div>;\n}\ntype Props = { n: number };\nenum Mode { On, Off }\n";
        let d = names("typescript", "App.tsx", tsx);
        assert!(has(&d, "App", "fn"), "{d:?}");
        assert!(has(&d, "Props", "type"), "{d:?}");
        assert!(has(&d, "Mode", "enum"), "{d:?}");
    }

    /// Code being edited is often broken; what still parses still counts.
    #[test]
    fn broken_code_keeps_the_definitions_that_parse() {
        let rs = "fn fine() {}\n\nfn broken( {\n\nstruct After;\n";
        let d = names("rust", "a.rs", rs);
        assert!(has(&d, "fine", "fn"), "{d:?}");
        let py = "def ok():\n    return 1\n\ndef bad(:\n    pass\n";
        let d = names("python", "a.py", py);
        assert!(has(&d, "ok", "fn"), "{d:?}");
    }

    #[test]
    fn an_empty_file_defines_nothing() {
        for (lang, path) in [
            ("rust", "a.rs"),
            ("python", "a.py"),
            ("javascript", "a.js"),
            ("typescript", "a.ts"),
            ("go", "a.go"),
        ] {
            let p = parse(lang, path, "", &[]).expect(lang);
            assert!(p.defs.is_empty() && p.imports.is_empty(), "{lang}");
        }
    }

    #[test]
    fn keywords_are_not_identifiers() {
        let p = parse(
            "python",
            "a.py",
            "def f(self):\n    return self.value\n",
            &["self"],
        )
        .unwrap();
        assert!(p.ids.contains("value"), "{:?}", p.ids);
        assert!(!p.ids.contains("self"), "{:?}", p.ids);
    }

    #[test]
    fn covered_languages_are_the_ones_with_grammars() {
        for l in ["rust", "python", "javascript", "typescript", "go"] {
            assert!(covers(l), "{l}");
        }
        for l in ["java", "c", "ruby", ""] {
            assert!(!covers(l), "{l}");
        }
    }
}
