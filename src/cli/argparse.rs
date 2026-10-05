//! A small, faithful re-implementation of the parts of Python's `argparse`
//! that the 1.x CLI used, so that parsing decisions, usage lines, help text
//! and error messages stay byte-for-byte identical (for non-colour output).
//!
//! It follows CPython 3.14's `argparse` algorithm: option classification
//! (exact, `--opt=value`, unique-prefix abbreviations, single-dash bundles,
//! negative numbers, strings containing spaces), alternating consumption of
//! optionals and positionals using the same nargs patterns, a sub-command
//! positional that swallows the rest of the command line, required-argument
//! checks, `unrecognized arguments` reporting, and the `HelpFormatter` layout
//! rules including terminal-width wrapping.

use std::collections::HashMap;

use super::repr as str_repr;
use tongs::python::{parse_float, parse_int};

/// How many command-line strings an action consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nargs {
    /// Flags (`store_true`, help, version).
    Zero,
    /// Exactly one value (argparse's default `nargs=None`).
    One,
    /// Sub-command: one name followed by everything else (`argparse.PARSER`).
    Parser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Help,
    Version,
    Store,
    StoreTrue,
    Subcommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Str,
    Float,
    Int,
}

/// A parsed value.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    None,
    Str(String),
    Float(f64),
    Int(i128),
    Bool(bool),
}

pub struct Action {
    pub option_strings: Vec<&'static str>,
    pub dest: &'static str,
    pub nargs: Nargs,
    pub kind: Kind,
    pub ty: Type,
    pub help: Option<&'static str>,
    pub required: bool,
    pub default: Val,
}

impl Action {
    pub fn flag(
        option_strings: &[&'static str],
        dest: &'static str,
        kind: Kind,
        help: &'static str,
    ) -> Self {
        Action {
            option_strings: option_strings.to_vec(),
            dest,
            nargs: Nargs::Zero,
            kind,
            ty: Type::Str,
            help: Some(help),
            required: false,
            default: if kind == Kind::StoreTrue {
                Val::Bool(false)
            } else {
                Val::None
            },
        }
    }

    pub fn option(
        option: &'static str,
        dest: &'static str,
        ty: Type,
        default: Val,
        help: Option<&'static str>,
    ) -> Self {
        Action {
            option_strings: vec![option],
            dest,
            nargs: Nargs::One,
            kind: Kind::Store,
            ty,
            help,
            required: false,
            default,
        }
    }

    pub fn positional(dest: &'static str, help: Option<&'static str>) -> Self {
        Action {
            option_strings: Vec::new(),
            dest,
            nargs: Nargs::One,
            kind: Kind::Store,
            ty: Type::Str,
            help,
            required: true,
            default: Val::None,
        }
    }

    fn is_positional(&self) -> bool {
        self.option_strings.is_empty()
    }

    /// `_get_action_name`.
    fn name(&self) -> String {
        if !self.option_strings.is_empty() {
            self.option_strings.join("/")
        } else {
            self.dest.to_owned()
        }
    }
}

pub struct SubCommand {
    pub name: &'static str,
    pub help: &'static str,
    pub parser: Parser,
}

pub struct Parser {
    pub prog: String,
    pub description: Option<&'static str>,
    pub version: Option<String>,
    pub actions: Vec<Action>,
    pub subcommands: Vec<SubCommand>,
}

pub type Namespace = HashMap<&'static str, Val>;

/// Parsing stopped: print to stdout/stderr and exit with `code`.
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

struct ArgError {
    action: Option<String>,
    message: String,
}

impl ArgError {
    fn new(action: Option<&Action>, message: String) -> Self {
        ArgError {
            action: action.map(Action::name),
            message,
        }
    }

    fn render(&self) -> String {
        match &self.action {
            Some(name) => format!("argument {name}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

enum Stop {
    Error(ArgError),
    Exit(Exit),
}

impl From<ArgError> for Stop {
    fn from(e: ArgError) -> Self {
        Stop::Error(e)
    }
}

/// One interpretation of an option-looking string:
/// (action index, option string, separator, explicit argument).
#[derive(Clone)]
struct OptTuple {
    action: Option<usize>,
    option_string: String,
    sep: Option<String>,
    explicit: Option<String>,
}

fn char_at(s: &str, i: usize) -> Option<char> {
    s.chars().nth(i)
}

fn looks_negative(arg: &str) -> bool {
    // CPython 3.14: re.compile(r'-\.?\d').match
    let mut chars = arg.chars();
    if chars.next() != Some('-') {
        return false;
    }
    match chars.next() {
        Some('.') => chars
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c.is_numeric()),
        Some(c) => c.is_ascii_digit() || c.is_numeric(),
        None => false,
    }
}

impl Parser {
    fn option_index(&self) -> Vec<(&'static str, usize)> {
        let mut out = Vec::new();
        for (i, a) in self.actions.iter().enumerate() {
            for s in &a.option_strings {
                out.push((*s, i));
            }
        }
        out
    }

    fn find_option(&self, s: &str) -> Option<usize> {
        self.option_index()
            .into_iter()
            .find(|(o, _)| *o == s)
            .map(|(_, i)| i)
    }

    fn parse_optional(&self, arg: &str) -> Option<Vec<OptTuple>> {
        if arg.is_empty() || !arg.starts_with('-') {
            return None;
        }
        if let Some(i) = self.find_option(arg) {
            return Some(vec![OptTuple {
                action: Some(i),
                option_string: arg.to_owned(),
                sep: None,
                explicit: None,
            }]);
        }
        if arg.chars().count() == 1 {
            return None;
        }
        if let Some((opt, explicit)) = arg.split_once('=')
            && let Some(i) = self.find_option(opt)
        {
            return Some(vec![OptTuple {
                action: Some(i),
                option_string: opt.to_owned(),
                sep: Some("=".to_owned()),
                explicit: Some(explicit.to_owned()),
            }]);
        }
        let tuples = self.option_tuples(arg);
        if !tuples.is_empty() {
            return Some(tuples);
        }
        if looks_negative(arg) {
            // No option of these parsers looks like a negative number.
            return None;
        }
        if arg.contains(' ') {
            return None;
        }
        Some(vec![OptTuple {
            action: None,
            option_string: arg.to_owned(),
            sep: None,
            explicit: None,
        }])
    }

    fn option_tuples(&self, arg: &str) -> Vec<OptTuple> {
        let mut result = Vec::new();
        let (prefix, sep, explicit) = match arg.split_once('=') {
            Some((p, e)) => (p, Some("=".to_owned()), Some(e.to_owned())),
            None => (arg, None, None),
        };
        if char_at(arg, 1) == Some('-') {
            for (opt, i) in self.option_index() {
                if opt.starts_with(prefix) {
                    result.push(OptTuple {
                        action: Some(i),
                        option_string: opt.to_owned(),
                        sep: sep.clone(),
                        explicit: explicit.clone(),
                    });
                }
            }
        } else {
            let split = arg.char_indices().nth(2).map_or(arg.len(), |(i, _)| i);
            let short_prefix = &arg[..split];
            let short_explicit = &arg[split..];
            for (opt, i) in self.option_index() {
                if opt == short_prefix {
                    result.push(OptTuple {
                        action: Some(i),
                        option_string: opt.to_owned(),
                        sep: Some(String::new()),
                        explicit: Some(short_explicit.to_owned()),
                    });
                } else if opt.starts_with(prefix) {
                    result.push(OptTuple {
                        action: Some(i),
                        option_string: opt.to_owned(),
                        sep: sep.clone(),
                        explicit: explicit.clone(),
                    });
                }
            }
        }
        result
    }

    /// `parse_args`: parse everything or exit like argparse would.
    pub fn parse_args(&self, args: &[String], width: usize) -> Result<Namespace, Exit> {
        let mut ns = Namespace::new();
        match self.parse_known(args, &mut ns, width) {
            Ok(extras) if extras.is_empty() => Ok(ns),
            Ok(extras) => Err(self.error(
                &format!("unrecognized arguments: {}", extras.join(" ")),
                width,
            )),
            Err(exit) => Err(exit),
        }
    }

    /// `parse_known_args`, including the error-to-exit conversion.
    fn parse_known(
        &self,
        args: &[String],
        ns: &mut Namespace,
        width: usize,
    ) -> Result<Vec<String>, Exit> {
        for a in &self.actions {
            if a.kind != Kind::Help && a.kind != Kind::Version {
                ns.entry(a.dest).or_insert_with(|| a.default.clone());
            }
        }
        match self.parse_inner(args, ns, width) {
            Ok(extras) => Ok(extras),
            Err(Stop::Exit(exit)) => Err(exit),
            Err(Stop::Error(e)) => Err(self.error(&e.render(), width)),
        }
    }

    fn parse_inner(
        &self,
        args: &[String],
        ns: &mut Namespace,
        width: usize,
    ) -> Result<Vec<String>, Stop> {
        // Classify every string: 'O' option, 'A' argument, '-' for "--".
        let mut pattern: Vec<char> = Vec::with_capacity(args.len());
        let mut option_indices: HashMap<usize, Vec<OptTuple>> = HashMap::new();
        let mut after_dashes = false;
        for (i, arg) in args.iter().enumerate() {
            if after_dashes {
                pattern.push('A');
            } else if arg == "--" {
                pattern.push('-');
                after_dashes = true;
            } else {
                match self.parse_optional(arg) {
                    None => pattern.push('A'),
                    Some(t) => {
                        option_indices.insert(i, t);
                        pattern.push('O');
                    }
                }
            }
        }

        let mut seen: Vec<bool> = vec![false; self.actions.len()];
        let mut extras: Vec<String> = Vec::new();
        let mut positionals: Vec<usize> = (0..self.actions.len())
            .filter(|i| self.actions[*i].is_positional())
            .collect();

        let max_option_index = option_indices.keys().copied().max();
        let mut start = 0usize;
        if let Some(max_opt) = max_option_index {
            while start <= max_opt {
                let mut next_opt = start;
                while next_opt <= max_opt && !option_indices.contains_key(&next_opt) {
                    next_opt += 1;
                }
                if start != next_opt {
                    let end = self.consume_positionals(
                        start,
                        args,
                        &pattern,
                        &mut positionals,
                        &mut seen,
                        ns,
                        &mut extras,
                        width,
                    )?;
                    if end > start {
                        start = end;
                        continue;
                    }
                    start = end;
                }
                if !option_indices.contains_key(&start) {
                    extras.extend(args[start..next_opt].iter().cloned());
                    start = next_opt;
                }
                start = self.consume_optional(
                    start,
                    args,
                    &pattern,
                    &option_indices,
                    &mut seen,
                    ns,
                    &mut extras,
                    width,
                )?;
            }
        }
        let stop = self.consume_positionals(
            start,
            args,
            &pattern,
            &mut positionals,
            &mut seen,
            ns,
            &mut extras,
            width,
        )?;
        extras.extend(args[stop.min(args.len())..].iter().cloned());

        let missing: Vec<String> = self
            .actions
            .iter()
            .enumerate()
            .filter(|(i, a)| !seen[*i] && a.required)
            .map(|(_, a)| a.name())
            .collect();
        if !missing.is_empty() {
            return Err(ArgError::new(
                None,
                format!(
                    "the following arguments are required: {}",
                    missing.join(", ")
                ),
            )
            .into());
        }
        Ok(extras)
    }

    #[allow(clippy::too_many_arguments)]
    fn consume_optional(
        &self,
        start: usize,
        args: &[String],
        pattern: &[char],
        option_indices: &HashMap<usize, Vec<OptTuple>>,
        seen: &mut [bool],
        ns: &mut Namespace,
        extras: &mut Vec<String>,
        width: usize,
    ) -> Result<usize, Stop> {
        let tuples = &option_indices[&start];
        if tuples.len() > 1 {
            let matches: Vec<&str> = tuples.iter().map(|t| t.option_string.as_str()).collect();
            return Err(ArgError::new(
                None,
                format!(
                    "ambiguous option: {} could match {}",
                    args[start],
                    matches.join(", ")
                ),
            )
            .into());
        }
        let OptTuple {
            mut action,
            mut option_string,
            mut sep,
            mut explicit,
        } = tuples[0].clone();
        let mut queued: Vec<(usize, Vec<String>)> = Vec::new();
        let stop;
        loop {
            let Some(ai) = action else {
                extras.push(args[start].clone());
                return Ok(start + 1);
            };
            let act = &self.actions[ai];
            if let Some(exp) = explicit.clone() {
                let arg_count = match act.nargs {
                    Nargs::Zero => 0,
                    _ => 1,
                };
                if arg_count == 0 && char_at(&option_string, 1) != Some('-') && !exp.is_empty() {
                    if sep.as_deref().is_some_and(|s| !s.is_empty()) || exp.starts_with('-') {
                        return Err(ArgError::new(
                            Some(act),
                            format!("ignored explicit argument {}", str_repr(&exp)),
                        )
                        .into());
                    }
                    queued.push((ai, Vec::new()));
                    let first = exp.chars().next().unwrap_or('-');
                    let lead = option_string.chars().next().unwrap_or('-');
                    option_string = format!("{lead}{first}");
                    let rest = &exp[first.len_utf8()..];
                    if let Some(next) = self.find_option(&option_string) {
                        action = Some(next);
                        if rest.is_empty() {
                            sep = None;
                            explicit = None;
                        } else if let Some(r) = rest.strip_prefix('=') {
                            sep = Some("=".to_owned());
                            explicit = Some(r.to_owned());
                        } else {
                            sep = Some(String::new());
                            explicit = Some(rest.to_owned());
                        }
                    } else {
                        extras.push(format!("{lead}{exp}"));
                        stop = start + 1;
                        break;
                    }
                } else if arg_count == 1 {
                    queued.push((ai, vec![exp]));
                    stop = start + 1;
                    break;
                } else {
                    return Err(ArgError::new(
                        Some(act),
                        format!("ignored explicit argument {}", str_repr(&exp)),
                    )
                    .into());
                }
            } else {
                let first = start + 1;
                let count = match act.nargs {
                    Nargs::Zero => 0,
                    _ => {
                        if pattern.get(first) == Some(&'A') {
                            1
                        } else {
                            return Err(ArgError::new(
                                Some(act),
                                "expected one argument".to_owned(),
                            )
                            .into());
                        }
                    }
                };
                queued.push((ai, args[first..first + count].to_vec()));
                stop = first + count;
                break;
            }
        }
        for (ai, values) in queued {
            self.take_action(ai, values, seen, ns, extras, width)?;
        }
        Ok(stop)
    }

    /// Greedy equivalent of matching the concatenated nargs regexes
    /// `(-*A-*)` / `(-*A[-AO]*)`; returns per-positional counts.
    fn match_positionals(&self, list: &[usize], pattern: &[char]) -> Option<Vec<usize>> {
        let mut pos = 0;
        let mut counts = Vec::with_capacity(list.len());
        for &ai in list {
            let begin = pos;
            while pattern.get(pos) == Some(&'-') {
                pos += 1;
            }
            if pattern.get(pos) != Some(&'A') {
                return None;
            }
            pos += 1;
            match self.actions[ai].nargs {
                Nargs::Parser => pos = pattern.len(),
                _ => {
                    while pattern.get(pos) == Some(&'-') {
                        pos += 1;
                    }
                }
            }
            counts.push(pos - begin);
        }
        Some(counts)
    }

    #[allow(clippy::too_many_arguments)]
    fn consume_positionals(
        &self,
        mut start: usize,
        args: &[String],
        pattern: &[char],
        positionals: &mut Vec<usize>,
        seen: &mut [bool],
        ns: &mut Namespace,
        extras: &mut Vec<String>,
        width: usize,
    ) -> Result<usize, Stop> {
        let selected = &pattern[start.min(pattern.len())..];
        let mut counts = Vec::new();
        for i in (1..=positionals.len()).rev() {
            if let Some(c) = self.match_positionals(&positionals[..i], selected) {
                counts = c;
                break;
            }
        }
        let consumed: Vec<usize> = positionals.drain(..counts.len()).collect();
        for (ai, count) in consumed.into_iter().zip(counts) {
            let mut values: Vec<String> = args[start..start + count].to_vec();
            if self.actions[ai].nargs == Nargs::Parser {
                if pattern[start] == '-' {
                    values.remove(0);
                }
            } else if pattern[start..start + count].contains(&'-')
                && let Some(p) = values.iter().position(|v| v == "--")
            {
                values.remove(p);
            }
            start += count;
            self.take_action(ai, values, seen, ns, extras, width)?;
        }
        Ok(start)
    }

    fn convert(&self, act: &Action, s: &str) -> Result<Val, ArgError> {
        let bad =
            |ty: &str| ArgError::new(Some(act), format!("invalid {ty} value: {}", str_repr(s)));
        match act.ty {
            Type::Str => Ok(Val::Str(s.to_owned())),
            Type::Float => parse_float(s).map(Val::Float).ok_or_else(|| bad("float")),
            Type::Int => parse_int(s).map(Val::Int).ok_or_else(|| bad("int")),
        }
    }

    fn take_action(
        &self,
        ai: usize,
        values: Vec<String>,
        seen: &mut [bool],
        ns: &mut Namespace,
        extras: &mut Vec<String>,
        width: usize,
    ) -> Result<(), Stop> {
        seen[ai] = true;
        let act = &self.actions[ai];
        match act.kind {
            Kind::Help => Err(Stop::Exit(Exit {
                code: 0,
                stdout: self.format_help(width),
                stderr: String::new(),
            })),
            Kind::Version => {
                let text = self.version.clone().unwrap_or_default();
                let body = format_text(&text, width, 0);
                Err(Stop::Exit(Exit {
                    code: 0,
                    stdout: finish(body),
                    stderr: String::new(),
                }))
            }
            Kind::StoreTrue => {
                ns.insert(act.dest, Val::Bool(true));
                Ok(())
            }
            Kind::Store => {
                let v = self.convert(act, &values[0])?;
                ns.insert(act.dest, v);
                Ok(())
            }
            Kind::Subcommand => {
                let name = &values[0];
                let Some(sub) = self.subcommands.iter().find(|s| s.name == name.as_str()) else {
                    let choices: Vec<String> =
                        self.subcommands.iter().map(|s| str_repr(s.name)).collect();
                    return Err(ArgError::new(
                        Some(act),
                        format!(
                            "invalid choice: {} (choose from {})",
                            str_repr(name),
                            choices.join(", ")
                        ),
                    )
                    .into());
                };
                ns.insert(act.dest, Val::Str(name.clone()));
                let mut sub_ns = Namespace::new();
                let rest = sub.parser.parse_known(&values[1..], &mut sub_ns, width);
                match rest {
                    Ok(unrecognized) => {
                        ns.extend(sub_ns);
                        extras.extend(unrecognized);
                        Ok(())
                    }
                    Err(exit) => Err(Stop::Exit(exit)),
                }
            }
        }
    }

    /// `ArgumentParser.error`: usage plus `prog: error: message`, exit 2.
    pub fn error(&self, message: &str, width: usize) -> Exit {
        Exit {
            code: 2,
            stdout: String::new(),
            stderr: format!(
                "{}{}: error: {}\n",
                self.format_usage(width),
                self.prog,
                message
            ),
        }
    }

    // ── Help formatting (argparse.HelpFormatter) ────────────────────────

    fn metavar(&self, act: &Action) -> String {
        match act.kind {
            Kind::Subcommand => {
                let names: Vec<&str> = self.subcommands.iter().map(|s| s.name).collect();
                format!("{{{}}}", names.join(","))
            }
            _ if act.is_positional() => act.dest.to_owned(),
            _ => act.dest.to_uppercase(),
        }
    }

    fn format_args(&self, act: &Action) -> String {
        let m = self.metavar(act);
        match act.nargs {
            Nargs::Parser => format!("{m} ..."),
            _ => m,
        }
    }

    fn usage_parts(&self) -> (Vec<String>, Vec<String>) {
        let mut opts = Vec::new();
        let mut pos = Vec::new();
        for act in &self.actions {
            if act.is_positional() {
                pos.push(self.format_args(act));
            } else {
                let part = match act.nargs {
                    Nargs::Zero => act.option_strings[0].to_owned(),
                    _ => format!("{} {}", act.option_strings[0], self.format_args(act)),
                };
                opts.push(if act.required {
                    part
                } else {
                    format!("[{part}]")
                });
            }
        }
        (opts, pos)
    }

    fn usage_body(&self, width: usize) -> String {
        self.usage_with_prefix(width, "usage: ")
    }

    /// The `prog` prefix argparse gives sub-commands: this parser's usage
    /// (positionals only, no `usage: ` prefix), stripped.
    pub fn subcommand_prefix(&self, width: usize) -> String {
        let positional_only = Parser {
            prog: self.prog.clone(),
            description: None,
            version: None,
            actions: self
                .actions
                .iter()
                .filter(|a| a.is_positional() && a.kind != Kind::Subcommand)
                .map(|a| Action::positional(a.dest, None))
                .collect(),
            subcommands: Vec::new(),
        };
        positional_only
            .usage_with_prefix(width, "")
            .trim_matches(|c: char| c.is_whitespace())
            .to_owned()
    }

    fn usage_with_prefix(&self, width: usize, prefix: &str) -> String {
        let prog = self.prog.as_str();
        let (opt_parts, pos_parts) = self.usage_parts();
        let mut all = vec![prog.to_owned()];
        all.extend(opt_parts.iter().cloned());
        all.extend(pos_parts.iter().cloned());
        let usage = all.join(" ");
        let text_width = width as isize;
        if (prefix.len() + usage.chars().count()) as isize <= text_width {
            return format!("{prefix}{usage}");
        }
        let get_lines = |parts: &[String], indent: &str, with_prefix: bool| -> Vec<String> {
            let mut lines: Vec<String> = Vec::new();
            let mut line: Vec<&str> = Vec::new();
            let indent_len = indent.chars().count() as isize;
            let mut line_len = if with_prefix {
                prefix.len() as isize - 1
            } else {
                indent_len - 1
            };
            for part in parts {
                let part_len = part.chars().count() as isize;
                if line_len + 1 + part_len > text_width && !line.is_empty() {
                    lines.push(format!("{indent}{}", line.join(" ")));
                    line.clear();
                    line_len = indent_len - 1;
                }
                line.push(part);
                line_len += part_len + 1;
            }
            if !line.is_empty() {
                lines.push(format!("{indent}{}", line.join(" ")));
            }
            if with_prefix && let Some(first) = lines.first_mut() {
                *first = first.chars().skip(indent.chars().count()).collect();
            }
            lines
        };
        let prog_len = prog.chars().count();
        let lines: Vec<String> = if (prefix.len() + prog_len) as f64 <= 0.75 * text_width as f64 {
            let indent = " ".repeat(prefix.len() + prog_len + 1);
            if !opt_parts.is_empty() {
                let mut first = vec![prog.to_owned()];
                first.extend(opt_parts.iter().cloned());
                let mut lines = get_lines(&first, &indent, true);
                lines.extend(get_lines(&pos_parts, &indent, false));
                lines
            } else if !pos_parts.is_empty() {
                let mut first = vec![prog.to_owned()];
                first.extend(pos_parts.iter().cloned());
                get_lines(&first, &indent, true)
            } else {
                vec![prog.to_owned()]
            }
        } else {
            let indent = " ".repeat(prefix.len());
            let mut parts = opt_parts.clone();
            parts.extend(pos_parts.iter().cloned());
            let mut lines = get_lines(&parts, &indent, false);
            if lines.len() > 1 {
                lines = get_lines(&opt_parts, &indent, false);
                lines.extend(get_lines(&pos_parts, &indent, false));
            }
            let mut out = vec![prog.to_owned()];
            out.extend(lines);
            out
        };
        format!("{prefix}{}", lines.join("\n"))
    }

    /// `format_usage()`.
    pub fn format_usage(&self, width: usize) -> String {
        finish(format!("{}\n\n", self.usage_body(width)))
    }

    fn invocation(&self, act: &Action) -> String {
        if act.is_positional() {
            self.metavar(act)
        } else if act.nargs == Nargs::Zero {
            act.option_strings.join(", ")
        } else {
            format!(
                "{} {}",
                act.option_strings.join(", "),
                self.format_args(act)
            )
        }
    }

    /// `format_help()`.
    pub fn format_help(&self, width: usize) -> String {
        let max_help_position = 24usize.min(width.saturating_sub(20).max(4));
        // Compute the longest invocation (with indentation), as add_argument does.
        let mut max_len = 0usize;
        for act in &self.actions {
            max_len = max_len.max(self.invocation(act).chars().count() + 2);
            if act.kind == Kind::Subcommand {
                for sub in &self.subcommands {
                    max_len = max_len.max(sub.name.chars().count() + 4);
                }
            }
        }
        let help_position = (max_len + 2).min(max_help_position);
        let mut out = format!("{}\n\n", self.usage_body(width));
        if let Some(desc) = self.description {
            out.push_str(&format_text(desc, width, 0));
        }
        let positionals: Vec<&Action> = self.actions.iter().filter(|a| a.is_positional()).collect();
        let optionals: Vec<&Action> = self.actions.iter().filter(|a| !a.is_positional()).collect();
        for (heading, group) in [
            ("positional arguments", positionals),
            ("options", optionals),
        ] {
            if group.is_empty() {
                continue;
            }
            let mut items = String::new();
            for act in group {
                items.push_str(&format_action(
                    &self.invocation(act),
                    act.help,
                    2,
                    help_position,
                    width,
                ));
                if act.kind == Kind::Subcommand {
                    for sub in &self.subcommands {
                        items.push_str(&format_action(
                            sub.name,
                            Some(sub.help),
                            4,
                            help_position,
                            width,
                        ));
                    }
                }
            }
            out.push_str(&format!("\n{heading}:\n{items}\n"));
        }
        finish(out)
    }
}

/// `format_help()` post-processing: collapse 3+ newlines, strip, add one.
fn finish(text: String) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
        } else {
            if run > 0 {
                collapsed.push_str(if run >= 3 { "\n\n" } else { &"\n\n"[..run] });
            }
            run = 0;
            collapsed.push(c);
        }
    }
    let body = collapsed.trim_matches('\n');
    if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    }
}

fn format_action(
    header: &str,
    help: Option<&str>,
    indent: usize,
    help_position: usize,
    width: usize,
) -> String {
    let help_width = width.saturating_sub(help_position).max(11);
    let action_width = help_position.saturating_sub(indent + 2);
    let pad = " ".repeat(indent);
    let mut out = String::new();
    let help = help.filter(|h| !h.is_empty());

    let indent_first = match help {
        None => {
            out.push_str(&format!("{pad}{header}\n"));
            return out;
        }
        Some(_) if header.chars().count() <= action_width => {
            out.push_str(&format!("{pad}{header:<action_width$}  "));
            0
        }
        Some(_) => {
            out.push_str(&format!("{pad}{header}\n"));
            help_position
        }
    };
    let lines = wrap(&collapse_ws(help.unwrap_or_default()), help_width);
    if let Some((first, rest)) = lines.split_first() {
        out.push_str(&format!("{}{first}\n", " ".repeat(indent_first)));
        for line in rest {
            out.push_str(&format!("{}{line}\n", " ".repeat(help_position)));
        }
    }
    out
}

fn format_text(text: &str, width: usize, indent: usize) -> String {
    let text_width = width.saturating_sub(indent).max(11);
    let pad = " ".repeat(indent);
    let lines: Vec<String> = wrap(&collapse_ws(text), text_width.saturating_sub(indent))
        .into_iter()
        .map(|l| format!("{pad}{l}"))
        .collect();
    format!("{}\n\n", lines.join("\n"))
}

fn collapse_ws(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split into textwrap chunks: whitespace runs and words, breaking
/// hyphenated words after each hyphen (`cross-` `process`).
fn chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let letter = |c: Option<&char>| c.is_some_and(|c| c.is_alphabetic());
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ' ' {
            if !cur.is_empty() && !cur.ends_with(' ') {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(c);
            i += 1;
            continue;
        }
        if cur.ends_with(' ') {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c);
        if c == '-'
            && i >= 2
            && letter(chars.get(i - 1))
            && (letter(chars.get(i - 2))
                || (i >= 3 && chars[i - 2] == '-' && letter(chars.get(i - 3))))
            && letter(chars.get(i + 1))
            && (letter(chars.get(i + 2))
                || (chars.get(i + 2) == Some(&'-') && letter(chars.get(i + 3))))
        {
            out.push(std::mem::take(&mut cur));
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `textwrap.wrap(text, width)` for argparse help strings.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut chunks: Vec<String> = chunks(text);
    chunks.reverse();
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut cur: Vec<String> = Vec::new();
        let mut cur_len = 0usize;
        if !lines.is_empty() && chunks.last().is_some_and(|c| c.trim().is_empty()) {
            chunks.pop();
        }
        while let Some(chunk) = chunks.last() {
            let l = chunk.chars().count();
            if cur_len + l <= width {
                cur_len += l;
                cur.push(chunks.pop().unwrap_or_default());
            } else {
                break;
            }
        }
        if chunks.last().is_some_and(|c| c.chars().count() > width) {
            // textwrap's _handle_long_word with break_long_words=True.
            let space_left = width.saturating_sub(cur_len);
            let chars: Vec<char> = chunks.pop().unwrap_or_default().chars().collect();
            let mut end = space_left.min(chars.len());
            if let Some(h) = chars[..end].iter().rposition(|c| *c == '-')
                && h > 0
                && chars[..h].iter().any(|c| *c != '-')
            {
                end = h + 1;
            }
            cur.push(chars[..end].iter().collect());
            chunks.push(chars[end..].iter().collect());
        }
        if cur.last().is_some_and(|c| c.trim().is_empty()) {
            cur.pop();
        }
        if !cur.is_empty() {
            lines.push(cur.concat());
        }
    }
    lines
}

/// Terminal width as `shutil.get_terminal_size().columns`.
pub fn terminal_columns() -> usize {
    if let Ok(v) = std::env::var("COLUMNS")
        && let Some(n) = parse_int(&v)
        && n > 0
    {
        // Any width beyond this already fits every line on one row.
        return usize::try_from(n).unwrap_or(usize::MAX).min(1 << 30);
    }
    // SAFETY: TIOCGWINSZ writes a `winsize` into the provided struct and has no
    // other effects; stdout may not be a terminal, in which case it fails.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 {
        return usize::from(ws.ws_col);
    }
    80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_like_textwrap() {
        assert_eq!(
            wrap(
                "Inspect and edit a schema-versioned, cross-process locked JSON document.",
                30
            ),
            vec![
                "Inspect and edit a schema-",
                "versioned, cross-process",
                "locked JSON document."
            ]
        );
        assert_eq!(
            wrap("cross-process-locked 2.0.0", 20),
            vec!["cross-process-locked", "2.0.0"]
        );
        assert_eq!(
            wrap("seconds to wait for the store lock (default: 10)", 54),
            vec!["seconds to wait for the store lock (default: 10)"]
        );
    }

    #[test]
    fn finish_collapses_blank_runs() {
        assert_eq!(finish("\n\na\n\n\n\nb\n\n".to_owned()), "a\n\nb\n");
    }
}
