use std::collections::HashSet;

use anyhow::{Context, Result};
use scraper::{ElementRef, Html, Node, Selector};

use crate::codeforces::ProblemId;

pub struct Sample {
    pub input: String,
    pub answer: String,
}

pub enum Kind {
    Standard,
    Interactive,
}

pub struct Statement {
    pub title: String,
    pub limits: String,
    pub body: String,
    pub samples: Vec<Sample>,
    pub kind: Kind,
}

#[derive(Clone, Copy)]
enum Whitespace {
    Collapse,
    Keep,
}

// Statements write math as $$$...$$$ LaTeX; these are the macros that show up in practice.
// Longer names go first so \leq is not read as \le followed by q.
const LATEX: [(&str, &str); 28] = [
    ("$$$", ""),
    ("\\left", ""),
    ("\\right", ""),
    ("\\leq", "≤"),
    ("\\le", "≤"),
    ("\\geq", "≥"),
    ("\\ge", "≥"),
    ("\\neq", "≠"),
    ("\\ne", "≠"),
    ("\\cdot", "·"),
    ("\\ldots", "…"),
    ("\\dots", "…"),
    ("\\times", "×"),
    ("\\to", "→"),
    ("\\oplus", "⊕"),
    ("\\lfloor", "⌊"),
    ("\\rfloor", "⌋"),
    ("\\lceil", "⌈"),
    ("\\rceil", "⌉"),
    ("\\sum", "Σ"),
    ("\\max", "max"),
    ("\\min", "min"),
    ("\\bmod", "mod"),
    ("\\mod", "mod"),
    ("\\,", " "),
    ("\\texttt", ""),
    ("\\mathrm", ""),
    ("\\operatorname", ""),
];

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("selectors are literals")
}

pub fn parse(html: &str) -> Result<Statement> {
    let document = Html::parse_document(html);
    let statement = document
        .select(&selector(".problem-statement"))
        .next()
        .context("the page has no problem statement")?;

    let title = first_text(statement, ".header .title");
    let time = first_text(statement, ".header .time-limit").replace("time limit per test", "");
    let memory =
        first_text(statement, ".header .memory-limit").replace("memory limit per test", "");
    let limits = format!("{} · {}", time.trim(), memory.trim());

    let mut body = String::new();
    let mut kind = Kind::Standard;
    for child in statement.child_elements() {
        let class = child.value().attr("class").unwrap_or("");
        if class == "header" || class == "sample-tests" {
            continue;
        }
        for title in child.select(&selector(".section-title")) {
            if title.text().collect::<String>() == "Interaction" {
                kind = Kind::Interactive;
            }
        }
        text(child, Whitespace::Collapse, &mut body);
    }
    for (macro_, symbol) in LATEX {
        body = body.replace(macro_, symbol);
    }

    let mut samples = Vec::new();
    let inputs = selector(".sample-test .input pre");
    let outputs = selector(".sample-test .output pre");
    for (input, answer) in statement.select(&inputs).zip(statement.select(&outputs)) {
        samples.push(Sample {
            input: lines(input),
            answer: lines(answer),
        });
    }

    Ok(Statement {
        title,
        limits,
        body,
        samples,
        kind,
    })
}

fn first_text(element: ElementRef, css: &str) -> String {
    let Some(found) = element.select(&selector(css)).next() else {
        return String::new();
    };
    found.text().collect()
}

fn lines(pre: ElementRef) -> String {
    let mut out = String::new();
    text(pre, Whitespace::Keep, &mut out);
    let out = out.trim_matches(|c| c == '\n' || c == '\r' || c == ' ');
    format!("{out}\n")
}

fn text(element: ElementRef, whitespace: Whitespace, out: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(t) => match whitespace {
                Whitespace::Collapse => {
                    for c in t.chars() {
                        if !c.is_whitespace() {
                            out.push(c);
                        } else if !out.ends_with([' ', '\n']) {
                            out.push(' ');
                        }
                    }
                }
                Whitespace::Keep => out.push_str(t),
            },
            Node::Element(e) => {
                let Some(child) = ElementRef::wrap(child) else {
                    continue;
                };
                match e.name() {
                    "br" => out.push('\n'),
                    "li" => {
                        out.push_str("- ");
                        text(child, whitespace, out);
                        out.push('\n');
                    }
                    "p" => {
                        text(child, whitespace, out);
                        out.push_str("\n\n");
                    }
                    "div" => {
                        text(child, whitespace, out);
                        out.push('\n');
                    }
                    _ => text(child, whitespace, out),
                }
            }
            Node::Document
            | Node::Fragment
            | Node::Doctype(_)
            | Node::Comment(_)
            | Node::ProcessingInstruction(_) => {}
        }
    }
}

pub fn comment(prefix: &str, statement: &Statement, url: &str) -> String {
    let mut out = format!(
        "{prefix} {}\n{prefix} {}\n{prefix} {url}\n{prefix}\n",
        statement.title, statement.limits
    );
    let mut blank = false;
    for line in statement.body.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !blank {
                out.push_str(prefix);
                out.push('\n');
            }
            blank = true;
            continue;
        }
        blank = false;
        let mut column = 0;
        for word in line.split_whitespace() {
            if column > 0 && column + word.len() > 76 {
                out.push('\n');
                column = 0;
            }
            if column == 0 {
                out.push_str(prefix);
            }
            out.push(' ');
            out.push_str(word);
            column += word.chars().count() + 1;
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

pub fn ids(source: &str) -> Vec<ProblemId> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    let separators = |c: char| c.is_whitespace() || ",;\"'<>()[]".contains(c);
    for word in source.split(separators) {
        let id = match link(word) {
            Some(id) => id,
            None => match word.parse::<ProblemId>() {
                Ok(id) => id,
                Err(()) => continue,
            },
        };
        if seen.insert(id.clone()) {
            ids.push(id);
        }
    }
    ids
}

fn link(word: &str) -> Option<ProblemId> {
    let parts: Vec<&str> = word.split('/').collect();
    for window in parts.windows(4) {
        let (contest, index) = match window {
            ["problemset", "problem", contest, index] => (contest, index),
            ["contest", contest, "problem", index] => (contest, index),
            _ => continue,
        };
        let index = index.split(['?', '#']).next().unwrap_or(index);
        let id = format!("{contest}{index}");
        if let Ok(id) = id.parse() {
            return Some(id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_bare_ids_and_links_in_order() {
        let found = ids(r#"4A 1352C, junk 1200 A
            <a href="https://codeforces.com/problemset/problem/1374/B">x</a>
            https://codeforces.com/contest/1520/problem/F2?locale=en 4A"#);
        let found: Vec<String> = found.iter().map(ProblemId::to_string).collect();
        assert_eq!(found, ["4A", "1352C", "1374B", "1520F2"]);
    }

    #[test]
    fn parses_statement_and_samples() {
        let html = r#"<div class="problem-statement"><div class="header"><div class="title">A. Watermelon</div><div class="time-limit"><div class="property-title">time limit per test</div>1 second</div><div class="memory-limit"><div class="property-title">memory limit per test</div>64 megabytes</div></div><div><p>Weight $$$w$$$ with
            $$$1 \le w$$$.</p></div><div class="sample-tests"><div class="sample-test"><div class="input"><pre><div class="test-example-line">2</div><div class="test-example-line">3 7</div></pre></div><div class="output"><pre>
YES
</pre></div></div></div></div>"#;
        let statement = parse(html).unwrap();
        assert_eq!(statement.title, "A. Watermelon");
        assert_eq!(statement.limits, "1 second · 64 megabytes");
        assert!(statement.body.contains("Weight w with 1 ≤ w."));
        assert_eq!(statement.samples.len(), 1);
        assert_eq!(statement.samples[0].input, "2\n3 7\n");
        assert_eq!(statement.samples[0].answer, "YES\n");
    }
}
