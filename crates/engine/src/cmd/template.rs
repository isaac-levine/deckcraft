//! Templates: `{{name}}` tokens in a deck's text, listed and filled from data. For agents and
//! automation that produce many decks from one design (mail merge for slides).
//!
//! Tokens are matched across run boundaries within a paragraph, because PowerPoint splits runs
//! at spell-check and formatting changes, so `{{name}}` on disk is often `{{na` + `me}}`.

use std::collections::BTreeMap;
use std::sync::Arc;

use deckcraft_model::text::{Paragraph, RunKind, TextBody};
use deckcraft_model::{Shape, ShapeId, ShapeKind, Slide};
use serde_json::{Value, json};

use super::*;
use crate::{Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "template.fields", "Template Fields", [], None, "{slides?: [index]} → {fields: [{name, count, slides: [index]}]}: the {{name}} tokens in slide text, tables, groups and notes", has_doc, fields),
        cmd!(
            "template.fill",
            "Fill Template",
            [],
            None,
            "{data: {name: string|number|bool|null}, slides?: [index], missing?: keep|empty|error} → {replaced, missing: [name]}: replaces {{name}} tokens (a value's \\n stays literal text)",
            has_doc,
            fill
        ),
    ]
}

/// A token's position in a paragraph's concatenated text (byte offsets) and its name.
struct Token {
    start: usize,
    end: usize,
    name: String,
}

const OPEN: &str = "{{";
const CLOSE: &str = "}}";

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

/// Every `{{ name }}` in `text`, left to right, non-overlapping.
fn find_tokens(text: &str) -> Vec<Token> {
    let mut out = vec![];
    let mut i = 0;
    while let Some(rel) = text.get(i..).and_then(|t| t.find(OPEN)) {
        let start = i + rel;
        let after_open = start + OPEN.len();
        let Some(rel_close) = text.get(after_open..).and_then(|t| t.find(CLOSE)) else { break };
        let close = after_open + rel_close;
        let inner = text.get(after_open..close).map(str::trim).unwrap_or_default();
        if !inner.is_empty() && inner.chars().all(is_name_char) {
            out.push(Token { start, end: close + CLOSE.len(), name: inner.to_string() });
            i = close + CLOSE.len();
        } else {
            // `{{{a}}` or `{{not a name}}`: move one brace on and look again.
            i = start + 1;
        }
    }
    out
}

/// Maximal spans of consecutive text runs `[a, b)` in a paragraph. A break, field or equation
/// ends a span: a token never crosses one.
fn text_spans(para: &Paragraph) -> Vec<(usize, usize)> {
    let mut spans = vec![];
    let mut start = None;
    for (i, r) in para.runs.iter().enumerate() {
        match (r.kind == RunKind::Text, start) {
            (true, None) => start = Some(i),
            (false, Some(a)) => {
                spans.push((a, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(a) = start {
        spans.push((a, para.runs.len()));
    }
    spans
}

/// Tokens in one paragraph, span by span, with the span they belong to.
fn paragraph_tokens(para: &Paragraph) -> Vec<((usize, usize), Vec<Token>)> {
    text_spans(para)
        .into_iter()
        .filter_map(|(a, b)| {
            let text: String = para.runs.get(a..b)?.iter().map(|r| r.text.as_str()).collect();
            let toks = find_tokens(&text);
            (!toks.is_empty()).then_some(((a, b), toks))
        })
        .collect()
}

/// Replace `[start, end)` of the span `[a, b)` of runs with `with`. The replacement lands in the
/// run where the token starts and keeps that run's formatting; the token's tail is cut from the
/// following runs.
fn splice(para: &mut Paragraph, (a, b): (usize, usize), start: usize, end: usize, with: &str) {
    let mut off = 0;
    let mut placed = false;
    for i in a..b {
        let Some(run) = para.runs.get_mut(i) else { break };
        let len = run.text.len();
        let run_start = off;
        let run_end = off + len;
        off = run_end;
        if run_end <= start || run_start >= end {
            continue;
        }
        let lo = start.saturating_sub(run_start).min(len);
        let hi = (end - run_start).min(len);
        if !run.text.is_char_boundary(lo) || !run.text.is_char_boundary(hi) || lo > hi {
            continue;
        }
        if placed {
            run.text.replace_range(lo..hi, "");
        } else {
            run.text.replace_range(lo..hi, with);
            placed = true;
        }
    }
}

fn drop_empty_runs(para: &mut Paragraph) {
    para.runs.retain(|r| r.kind != RunKind::Text || !r.text.is_empty());
}

enum Missing {
    Keep,
    Empty,
    Error,
}

/// Fill one paragraph. Counts replacements; names without data go to `missing`.
fn fill_paragraph(para: &mut Paragraph, data: &BTreeMap<String, String>, mode: &Missing, missing: &mut Vec<String>) -> usize {
    let mut n = 0;
    // Later tokens first, so earlier byte offsets stay valid while text changes length.
    for (span, toks) in paragraph_tokens(para).into_iter().rev() {
        for t in toks.into_iter().rev() {
            match data.get(&t.name) {
                Some(v) => {
                    splice(para, span, t.start, t.end, v);
                    n += 1;
                }
                None => {
                    if !missing.contains(&t.name) {
                        missing.push(t.name.clone());
                    }
                    if matches!(mode, Missing::Empty) {
                        splice(para, span, t.start, t.end, "");
                        n += 1;
                    }
                }
            }
        }
    }
    if n > 0 {
        drop_empty_runs(para);
    }
    n
}

// ---------- traversal ----------

fn walk_bodies<'a>(shapes: &'a [Shape], depth: usize, f: &mut impl FnMut(&'a TextBody)) {
    if depth > 64 {
        return;
    }
    for sh in shapes {
        if let Some(b) = &sh.text {
            f(b);
        }
        match &sh.kind {
            ShapeKind::Group { children, .. } => walk_bodies(children, depth + 1, f),
            ShapeKind::Table(t) => t.rows.iter().flat_map(|r| r.cells.iter()).for_each(|c| f(&c.text)),
            _ => {}
        }
    }
}

fn slide_bodies<'a>(slide: &'a Slide, f: &mut impl FnMut(&'a TextBody)) {
    walk_bodies(&slide.shapes, 0, f);
    f(&slide.notes);
}

/// Mutable walk; `f` gets the body and the top-level shape it belongs to (`None` for notes), so
/// callers can refit autosized text boxes afterwards.
fn walk_bodies_mut(shapes: &mut [Shape], top: Option<ShapeId>, depth: usize, f: &mut impl FnMut(&mut TextBody, Option<ShapeId>)) {
    if depth > 64 {
        return;
    }
    for sh in shapes.iter_mut() {
        let top = top.or(Some(sh.id));
        if let Some(b) = &mut sh.text {
            f(b, top);
        }
        match &mut sh.kind {
            ShapeKind::Group { children, .. } => walk_bodies_mut(children, top, depth + 1, f),
            ShapeKind::Table(t) => t.rows.iter_mut().flat_map(|r| r.cells.iter_mut()).for_each(|c| f(&mut c.text, None)),
            _ => {}
        }
    }
}

fn slide_bodies_mut(slide: &mut Slide, f: &mut impl FnMut(&mut TextBody, Option<ShapeId>)) {
    walk_bodies_mut(&mut slide.shapes, None, 0, f);
    f(&mut slide.notes, None);
}

/// The slide indexes a command works on: `slides` or every slide. Out-of-range is an error.
fn slide_indexes(s: &Session, p: &Value, cmd: &str) -> Result<Vec<usize>> {
    let n = s.doc()?.doc.slides.len();
    match p.get("slides") {
        None | Some(Value::Null) => Ok((0..n).collect()),
        Some(Value::Array(a)) => {
            let mut out = vec![];
            for v in a {
                let i = v.as_u64().and_then(|x| usize::try_from(x).ok()).ok_or_else(|| bad(cmd, "`slides` must be slide indexes"))?;
                if i >= n {
                    return Err(bad(cmd, format!("no slide {i}")));
                }
                if !out.contains(&i) {
                    out.push(i);
                }
            }
            Ok(out)
        }
        Some(_) => Err(bad(cmd, "`slides` must be an array of indexes")),
    }
}

// ---------- commands ----------

fn fields(s: &mut Session, p: &Value) -> Result<Value> {
    let idx = slide_indexes(s, p, "template.fields")?;
    let st = s.doc()?;
    let mut order: Vec<String> = vec![];
    let mut counts: BTreeMap<String, (usize, Vec<usize>)> = BTreeMap::new();
    for i in idx {
        let Some(slide) = st.doc.slides.get(i) else { continue };
        slide_bodies(slide, &mut |b| {
            for para in &b.paragraphs {
                for (_, toks) in paragraph_tokens(para) {
                    for t in toks {
                        let e = counts.entry(t.name.clone()).or_insert_with(|| {
                            order.push(t.name.clone());
                            (0, vec![])
                        });
                        e.0 += 1;
                        if !e.1.contains(&i) {
                            e.1.push(i);
                        }
                    }
                }
            }
        });
    }
    let fields: Vec<Value> =
        order.iter().filter_map(|name| counts.get(name).map(|(count, slides)| json!({"name": name, "count": count, "slides": slides}))).collect();
    Ok(json!({"fields": fields}))
}

/// `data` as strings: scalars only, so a value never silently turns into `[object]`.
fn data_param(p: &Value) -> Result<BTreeMap<String, String>> {
    let obj = p.get("data").and_then(Value::as_object).ok_or_else(|| bad("template.fill", "missing `data` object"))?;
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        let text = match v {
            Value::String(t) => t.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
            Value::Array(_) | Value::Object(_) => return Err(bad("template.fill", format!("`data.{k}` must be a string, number, bool or null"))),
        };
        out.insert(k.clone(), text);
    }
    Ok(out)
}

fn fill(s: &mut Session, p: &Value) -> Result<Value> {
    let data = data_param(p)?;
    let mode = match str_param(p, "missing").unwrap_or("keep") {
        "keep" => Missing::Keep,
        "empty" => Missing::Empty,
        "error" => Missing::Error,
        other => return Err(bad("template.fill", format!("`missing` must be keep, empty or error, not `{other}`"))),
    };
    let idx = slide_indexes(s, p, "template.fill")?;
    if matches!(mode, Missing::Error) {
        // Validate before touching the deck, so an error leaves it exactly as it was.
        let st = s.doc()?;
        let mut missing: Vec<String> = vec![];
        for i in &idx {
            let Some(slide) = st.doc.slides.get(*i) else { continue };
            slide_bodies(slide, &mut |b| {
                for para in &b.paragraphs {
                    for (_, toks) in paragraph_tokens(para) {
                        for t in toks {
                            if !data.contains_key(&t.name) && !missing.contains(&t.name) {
                                missing.push(t.name);
                            }
                        }
                    }
                }
            });
        }
        if !missing.is_empty() {
            return Err(bad("template.fill", format!("no data for {}", missing.join(", "))));
        }
    }
    let mut missing: Vec<String> = vec![];
    let mut changed: Vec<(usize, ShapeId)> = vec![];
    let replaced = s.edit(|doc, _| {
        let mut n = 0;
        for i in &idx {
            let Some(slide) = doc.slides.get_mut(*i) else { continue };
            let slide = Arc::make_mut(slide);
            slide_bodies_mut(slide, &mut |b, top| {
                let k: usize = b.paragraphs.iter_mut().map(|para| fill_paragraph(para, &data, &mode, &mut missing)).sum();
                n += k;
                if k > 0
                    && let Some(id) = top
                {
                    changed.push((*i, id));
                }
            });
        }
        Ok(n)
    })?;
    refit(s, &changed)?;
    Ok(json!({"replaced": replaced, "missing": missing}))
}

/// Autosized text boxes grow or shrink to their new text. `fit_text_box` works on the current
/// slide, so visit each changed slide in turn and put the selection back afterwards. All of this
/// is still one command, so it is one undo step.
fn refit(s: &mut Session, changed: &[(usize, ShapeId)]) -> Result<()> {
    if changed.is_empty() {
        return Ok(());
    }
    let orig = s.doc()?.selection.clone();
    for (i, id) in changed {
        s.select(|_, sel| {
            sel.target = crate::Target::Slides;
            sel.slide = *i;
        })?;
        text::fit_text_box(s, *id)?;
    }
    s.select(|_, sel| *sel = orig)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_found_with_spaces_and_odd_braces() {
        let t = find_tokens("Hi {{ first_name }}, {{{x}}} and {{not a name}} {{a.b-c}}");
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["first_name", "x", "a.b-c"]);
        assert_eq!(find_tokens("{{").len(), 0);
        assert_eq!(find_tokens("}}{{").len(), 0);
        assert_eq!(find_tokens("{{}}").len(), 0);
    }

    #[test]
    fn spans_stop_at_breaks() {
        let mut p = Paragraph::new("a");
        p.runs.push(deckcraft_model::text::Run { text: String::new(), props: Default::default(), kind: RunKind::Break });
        p.runs.push(deckcraft_model::text::Run::new("b"));
        assert_eq!(text_spans(&p), vec![(0, 1), (2, 3)]);
    }
}
