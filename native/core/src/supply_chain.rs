//! New packages before they are installed, and what a lockfile change adds.
//!
//! When an action would add dependencies (`npm install x`, `pip install x`,
//! `cargo add x`, …, or an edit that adds them to `package.json`,
//! `requirements.txt`, `pyproject.toml`, `Cargo.toml` or `go.mod`), each new
//! package is looked up before the user approves it:
//! - it must exist on its registry (npm, PyPI, crates.io);
//! - one first published under 30 days ago is pointed out;
//! - a name one or two typos away from a popular package (or the same name
//!   with other separators) is pointed out as a possible look-alike.
//!
//! Lookups are short (a few seconds), cached for a day, and use no account.
//! Offline or on an error a package is "not checked", never blocked. The
//! popular names are a small built-in list of widely used packages per
//! registry (see `POPULAR_*`); refresh them from each registry's download
//! statistics when a release changes them.
use crate::store::Store;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

/// Packages younger than this are pointed out.
const NEW_DAYS: f64 = 30.0;
/// Cached answers are used for this long.
const CACHE_SECONDS: f64 = 24.0 * 3600.0;
/// Most packages looked up for one action.
const MAX_LOOKUPS: usize = 12;
/// Largest registry answer read.
const MAX_ANSWER_BYTES: usize = 3 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ecosystem {
    Npm,
    PyPI,
    Crates,
    Go,
    RubyGems,
}
impl Ecosystem {
    pub fn label(self) -> &'static str {
        match self {
            Ecosystem::Npm => "npm",
            Ecosystem::PyPI => "PyPI",
            Ecosystem::Crates => "crates.io",
            Ecosystem::Go => "Go",
            Ecosystem::RubyGems => "RubyGems",
        }
    }
    fn id(self) -> &'static str {
        match self {
            Ecosystem::Npm => "npm",
            Ecosystem::PyPI => "pypi",
            Ecosystem::Crates => "crates",
            Ecosystem::Go => "go",
            Ecosystem::RubyGems => "rubygems",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Package {
    pub ecosystem: Ecosystem,
    pub name: String,
}

/// What a lookup found.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// On the registry, older than 30 days.
    Known { age_days: Option<f64> },
    /// On the registry, first published recently.
    New { age_days: f64 },
    /// Not on the registry.
    Missing,
    /// Could not be checked (offline, an error, an unsupported registry).
    Unchecked(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub package: Package,
    pub status: Status,
    /// A popular package this name closely resembles.
    pub lookalike: Option<String>,
}

// ---------- which packages an action adds ----------

/// The name without a version or extras: `left-pad@1.3.0` → `left-pad`,
/// `@scope/pkg@^2` → `@scope/pkg`, `requests[socks]>=2` → `requests`.
fn bare_name(ecosystem: Ecosystem, spec: &str) -> Option<String> {
    let spec = spec.trim().trim_matches(|c| c == '"' || c == '\'');
    if spec.is_empty()
        || spec.starts_with('-')
        || spec.starts_with('.')
        || spec.starts_with('/')
        || spec.starts_with('~')
        || spec.contains("://")
        || spec.starts_with("git+")
        || spec.starts_with("file:")
        || spec.starts_with("link:")
        || spec.starts_with("workspace:")
        || spec.ends_with(".txt")
        || spec.ends_with(".whl")
        || spec.ends_with(".tar.gz")
        || spec.ends_with(".tgz")
    {
        return None;
    }
    let name = match ecosystem {
        Ecosystem::Npm => {
            let (scope, rest) = match spec.strip_prefix('@') {
                Some(rest) => ("@", rest),
                None => ("", spec),
            };
            let rest = rest.split('@').next().unwrap_or(rest);
            format!("{scope}{rest}")
        }
        Ecosystem::PyPI => spec
            .split(|c: char| "[<>=!~;@ ".contains(c))
            .next()
            .unwrap_or(spec)
            .to_owned(),
        Ecosystem::Crates => spec.split('@').next().unwrap_or(spec).to_owned(),
        Ecosystem::Go => spec.split('@').next().unwrap_or(spec).to_owned(),
        Ecosystem::RubyGems => spec.split(':').next().unwrap_or(spec).to_owned(),
    };
    let valid = !name.is_empty()
        && name.len() <= 214
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@/._-".contains(c));
    valid.then_some(name)
}

/// Packages a shell command would add to the project.
pub fn from_command(command: &str) -> Vec<Package> {
    let script = crate::approvals::shell::parse(command);
    let mut found = BTreeSet::new();
    for simple in &script.commands {
        let words: Vec<&str> = simple.words.iter().map(|w| w.text.as_str()).collect();
        // Skip wrappers such as sudo, env, time.
        let start = words
            .iter()
            .position(|w| {
                !matches!(
                    w.rsplit('/').next().unwrap_or(w),
                    "sudo" | "doas" | "env" | "time" | "nohup" | "command" | "exec"
                ) && !w.contains('=')
            })
            .unwrap_or(0);
        let words = &words[start..];
        let Some(program) = words.first().map(|w| w.rsplit('/').next().unwrap_or(w)) else {
            continue;
        };
        let args = &words[1..];
        let action = args
            .iter()
            .find(|a| !a.starts_with('-'))
            .copied()
            .unwrap_or("");
        let after_action = || {
            args.iter()
                .skip_while(|a| **a != action)
                .skip(1)
                .copied()
                .collect::<Vec<_>>()
        };
        let (ecosystem, specs): (Ecosystem, Vec<&str>) = match program {
            "npm" | "pnpm" | "yarn" | "bun" if matches!(action, "install" | "i" | "add" | "in") => {
                (Ecosystem::Npm, after_action())
            }
            "pip" | "pip3" | "pipx" if action == "install" => (Ecosystem::PyPI, after_action()),
            "python" | "python3"
                if args.first() == Some(&"-m")
                    && args.get(1) == Some(&"pip")
                    && args.get(2) == Some(&"install") =>
            {
                (Ecosystem::PyPI, args[3..].to_vec())
            }
            "uv" if action == "add" => (Ecosystem::PyPI, after_action()),
            "uv" if action == "pip" && args.contains(&"install") => (
                Ecosystem::PyPI,
                args.iter()
                    .skip_while(|a| **a != "install")
                    .skip(1)
                    .copied()
                    .collect(),
            ),
            "poetry" if action == "add" => (Ecosystem::PyPI, after_action()),
            "cargo" if action == "add" => (Ecosystem::Crates, after_action()),
            "cargo" if action == "install" => (Ecosystem::Crates, after_action()),
            "go" if matches!(action, "get" | "install") => (Ecosystem::Go, after_action()),
            "gem" if action == "install" => (Ecosystem::RubyGems, after_action()),
            _ => continue,
        };
        // Options that take a value are skipped with it.
        let mut skip_next = false;
        for spec in specs {
            if skip_next {
                skip_next = false;
                continue;
            }
            if matches!(
                spec,
                "-r" | "--requirement"
                    | "-c"
                    | "--constraint"
                    | "-e"
                    | "--editable"
                    | "--index-url"
                    | "-i"
                    | "--extra-index-url"
                    | "--registry"
                    | "--features"
                    | "-F"
                    | "--path"
                    | "--git"
                    | "--branch"
                    | "--tag"
                    | "--rev"
                    | "--target"
                    | "-t"
                    | "--prefix"
                    | "--version"
                    | "--vers"
                    | "--group"
                    | "-G"
            ) {
                skip_next = true;
                continue;
            }
            if let Some(name) = bare_name(ecosystem, spec) {
                found.insert(Package { ecosystem, name });
            }
        }
    }
    found.into_iter().collect()
}

fn names_in_json_deps(text: &str) -> BTreeSet<String> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return BTreeSet::new();
    };
    let mut names = BTreeSet::new();
    for key in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(map) = value[key].as_object() {
            for (name, version) in map {
                let local = version.as_str().is_some_and(|v| {
                    v.starts_with("file:")
                        || v.starts_with("link:")
                        || v.starts_with("workspace:")
                        || v.contains("://")
                        || v.starts_with("git+")
                });
                if !local {
                    names.insert(name.clone());
                }
            }
        }
    }
    names
}

fn names_in_requirements(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty() && !l.starts_with('-'))
        .filter_map(|l| bare_name(Ecosystem::PyPI, l))
        .collect()
}

/// Registry dependencies in one TOML dependency table (path and Git
/// dependencies are left out).
fn add_toml_table(names: &mut BTreeSet<String>, table: Option<&toml::Value>) {
    let Some(table) = table.and_then(|t| t.as_table()) else {
        return;
    };
    for (name, spec) in table {
        let local = spec.as_table().is_some_and(|t| {
            t.contains_key("path") || t.contains_key("git") || t.get("workspace").is_some()
        });
        if !local && name != "python" {
            let name = spec
                .as_table()
                .and_then(|t| t.get("package"))
                .and_then(|p| p.as_str())
                .unwrap_or(name);
            names.insert(name.to_owned());
        }
    }
}

fn add_pep508(names: &mut BTreeSet<String>, list: Option<&toml::Value>) {
    for spec in list
        .and_then(|l| l.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
    {
        if let Some(name) = bare_name(Ecosystem::PyPI, spec) {
            names.insert(name);
        }
    }
}

fn names_in_toml(text: &str, ecosystem: Ecosystem) -> BTreeSet<String> {
    let Ok(value) = text.parse::<toml::Table>() else {
        return BTreeSet::new();
    };
    let mut names = BTreeSet::new();
    const KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    match ecosystem {
        Ecosystem::Crates => {
            for key in KINDS {
                add_toml_table(&mut names, value.get(key));
            }
            add_toml_table(
                &mut names,
                value.get("workspace").and_then(|w| w.get("dependencies")),
            );
            if let Some(targets) = value.get("target").and_then(|t| t.as_table()) {
                for target in targets.values() {
                    for key in KINDS {
                        add_toml_table(&mut names, target.get(key));
                    }
                }
            }
        }
        _ => {
            let project = value.get("project");
            add_pep508(&mut names, project.and_then(|p| p.get("dependencies")));
            if let Some(groups) = project
                .and_then(|p| p.get("optional-dependencies"))
                .and_then(|d| d.as_table())
            {
                for list in groups.values() {
                    add_pep508(&mut names, Some(list));
                }
            }
            add_toml_table(
                &mut names,
                value
                    .get("tool")
                    .and_then(|t| t.get("poetry"))
                    .and_then(|p| p.get("dependencies")),
            );
        }
    }
    names
}

fn names_in_go_mod(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut block = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("require (") {
            block = true;
            continue;
        }
        if block && line.starts_with(')') {
            block = false;
            continue;
        }
        let spec = if block {
            Some(line)
        } else {
            line.strip_prefix("require ")
        };
        if let Some(module) = spec.and_then(|s| s.split_whitespace().next()) {
            if module.contains('.') && !module.starts_with("//") {
                names.insert(module.to_owned());
            }
        }
    }
    names
}

/// The manifest kind of a path, if it is one.
pub fn manifest(path: &str) -> Option<Ecosystem> {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "package.json" => Some(Ecosystem::Npm),
        "pyproject.toml" => Some(Ecosystem::PyPI),
        "Cargo.toml" => Some(Ecosystem::Crates),
        "go.mod" => Some(Ecosystem::Go),
        _ if name.starts_with("requirements") && name.ends_with(".txt") => Some(Ecosystem::PyPI),
        _ => None,
    }
}

/// Packages an edit to a manifest adds: in `after` but not in `before`.
pub fn from_manifest(path: &str, before: Option<&str>, after: &str) -> Vec<Package> {
    let Some(ecosystem) = manifest(path) else {
        return Vec::new();
    };
    let names = |text: &str| -> BTreeSet<String> {
        let name = path.rsplit('/').next().unwrap_or(path);
        match name {
            "package.json" => names_in_json_deps(text),
            "Cargo.toml" => names_in_toml(text, Ecosystem::Crates),
            "pyproject.toml" => names_in_toml(text, Ecosystem::PyPI),
            "go.mod" => names_in_go_mod(text),
            _ => names_in_requirements(text),
        }
    };
    let old = before.map(names).unwrap_or_default();
    names(after)
        .into_iter()
        .filter(|n| !old.contains(n))
        .map(|name| Package { ecosystem, name })
        .collect()
}

// ---------- look-alikes ----------

const POPULAR_NPM: &[&str] = &[
    "lodash",
    "react",
    "react-dom",
    "express",
    "axios",
    "chalk",
    "commander",
    "debug",
    "moment",
    "request",
    "typescript",
    "webpack",
    "vue",
    "jquery",
    "uuid",
    "dotenv",
    "yargs",
    "async",
    "bluebird",
    "underscore",
    "fs-extra",
    "glob",
    "minimist",
    "semver",
    "colors",
    "body-parser",
    "classnames",
    "prop-types",
    "rxjs",
    "tslib",
    "core-js",
    "eslint",
    "prettier",
    "jest",
    "mocha",
    "chai",
    "@babel/core",
    "next",
    "redux",
    "react-redux",
    "styled-components",
    "mongoose",
    "mysql",
    "mysql2",
    "pg",
    "redis",
    "socket.io",
    "cors",
    "jsonwebtoken",
    "bcrypt",
    "bcryptjs",
    "nodemon",
    "ws",
    "node-fetch",
    "cross-env",
    "rimraf",
    "mkdirp",
    "inquirer",
    "ora",
    "yaml",
    "zod",
    "vite",
    "esbuild",
    "rollup",
    "vitest",
    "@types/node",
    "@types/react",
    "date-fns",
    "dayjs",
    "luxon",
    "ramda",
    "immer",
    "graphql",
    "prisma",
    "@prisma/client",
    "sequelize",
    "knex",
    "sharp",
    "puppeteer",
    "playwright",
    "cheerio",
    "handlebars",
    "ejs",
    "pug",
    "marked",
    "highlight.js",
    "three",
    "d3",
    "chart.js",
    "electron",
    "tailwindcss",
    "postcss",
    "autoprefixer",
    "sass",
    "less",
    "lint-staged",
    "husky",
    "concurrently",
    "cross-spawn",
    "execa",
    "got",
    "superagent",
    "form-data",
    "qs",
    "cookie-parser",
    "helmet",
    "morgan",
    "winston",
    "pino",
    "validator",
    "joi",
    "yup",
    "ajv",
    "nanoid",
    "crypto-js",
    "bignumber.js",
    "iconv-lite",
    "formik",
    "react-hook-form",
    "react-router",
    "react-router-dom",
    "@reduxjs/toolkit",
    "zustand",
    "swr",
    "@tanstack/react-query",
    "antd",
    "@mui/material",
    "bootstrap",
    "framer-motion",
    "chokidar",
    "http-proxy",
    "express-session",
    "passport",
    "multer",
    "nodemailer",
    "stripe",
    "aws-sdk",
    "firebase",
    "openai",
    "@anthropic-ai/sdk",
    "socket.io-client",
    "koa",
    "fastify",
    "hapi",
    "nestjs",
    "@nestjs/core",
    "svelte",
    "angular",
    "@angular/core",
    "lerna",
    "nx",
    "turbo",
    "babel-loader",
    "css-loader",
    "style-loader",
    "html-webpack-plugin",
    "ts-node",
    "tsx",
    "ts-jest",
    "@testing-library/react",
    "cypress",
];
const POPULAR_PYPI: &[&str] = &[
    "requests",
    "numpy",
    "pandas",
    "scipy",
    "matplotlib",
    "django",
    "flask",
    "fastapi",
    "uvicorn",
    "pydantic",
    "sqlalchemy",
    "pytest",
    "setuptools",
    "wheel",
    "pip",
    "six",
    "urllib3",
    "certifi",
    "idna",
    "charset-normalizer",
    "python-dateutil",
    "pytz",
    "pyyaml",
    "jinja2",
    "markupsafe",
    "click",
    "attrs",
    "boto3",
    "botocore",
    "s3transfer",
    "cryptography",
    "pyopenssl",
    "cffi",
    "packaging",
    "typing-extensions",
    "tqdm",
    "pillow",
    "scikit-learn",
    "tensorflow",
    "torch",
    "torchvision",
    "transformers",
    "keras",
    "beautifulsoup4",
    "lxml",
    "selenium",
    "scrapy",
    "celery",
    "redis",
    "psycopg2",
    "psycopg2-binary",
    "pymongo",
    "mysqlclient",
    "aiohttp",
    "httpx",
    "websockets",
    "gunicorn",
    "black",
    "flake8",
    "pylint",
    "mypy",
    "isort",
    "coverage",
    "tox",
    "virtualenv",
    "poetry",
    "pipenv",
    "rich",
    "typer",
    "loguru",
    "openpyxl",
    "sphinx",
    "jsonschema",
    "toml",
    "tomli",
    "protobuf",
    "grpcio",
    "opencv-python",
    "seaborn",
    "plotly",
    "dash",
    "streamlit",
    "gradio",
    "openai",
    "anthropic",
    "langchain",
    "tiktoken",
    "huggingface-hub",
    "datasets",
    "nltk",
    "spacy",
    "networkx",
    "sympy",
    "statsmodels",
    "xgboost",
    "lightgbm",
    "joblib",
    "paramiko",
    "ansible",
    "docker",
    "kubernetes",
    "pyjwt",
    "passlib",
    "bcrypt",
    "python-dotenv",
    "colorama",
    "tabulate",
    "arrow",
    "pendulum",
    "regex",
    "simplejson",
    "ujson",
    "orjson",
    "marshmallow",
    "alembic",
    "werkzeug",
    "pyparsing",
    "filelock",
    "platformdirs",
    "importlib-metadata",
    "wrapt",
    "decorator",
    "pluggy",
    "more-itertools",
    "greenlet",
    "gevent",
    "ruff",
    "uv",
    "httpcore",
];
const POPULAR_CRATES: &[&str] = &[
    "serde",
    "serde_json",
    "serde_derive",
    "tokio",
    "anyhow",
    "thiserror",
    "rand",
    "regex",
    "clap",
    "log",
    "env_logger",
    "tracing",
    "tracing-subscriber",
    "reqwest",
    "hyper",
    "futures",
    "async-trait",
    "chrono",
    "time",
    "uuid",
    "lazy_static",
    "once_cell",
    "itertools",
    "bytes",
    "bitflags",
    "libc",
    "syn",
    "quote",
    "proc-macro2",
    "cfg-if",
    "base64",
    "sha2",
    "hex",
    "url",
    "http",
    "tower",
    "axum",
    "actix-web",
    "rocket",
    "warp",
    "diesel",
    "sqlx",
    "rusqlite",
    "toml",
    "serde_yaml",
    "walkdir",
    "tempfile",
    "glob",
    "crossbeam",
    "rayon",
    "parking_lot",
    "num",
    "num-traits",
    "smallvec",
    "indexmap",
    "hashbrown",
    "memchr",
    "nom",
    "byteorder",
    "flate2",
    "zip",
    "tar",
    "image",
    "criterion",
    "proptest",
    "mockall",
    "tokio-util",
    "dashmap",
    "crossterm",
    "ratatui",
    "colored",
    "indicatif",
    "dirs",
    "directories",
    "which",
    "semver",
    "strum",
    "getrandom",
    "ring",
    "rustls",
    "openssl",
    "native-tls",
    "tonic",
    "prost",
    "wasm-bindgen",
    "tauri",
    "egui",
];

fn popular(ecosystem: Ecosystem) -> &'static [&'static str] {
    match ecosystem {
        Ecosystem::Npm => POPULAR_NPM,
        Ecosystem::PyPI => POPULAR_PYPI,
        Ecosystem::Crates => POPULAR_CRATES,
        _ => &[],
    }
}

/// Lowercase, with `-`, `_` and `.` removed (PyPI and crates.io treat
/// them as the same, and they are an easy way to fake a name).
fn folded(name: &str) -> String {
    name.to_ascii_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | '.'))
        .collect()
}

/// Optimal string alignment distance (insertions, deletions, substitutions
/// and swaps of neighbours).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// A popular package `name` closely resembles, if it is not one itself.
pub fn lookalike(ecosystem: Ecosystem, name: &str) -> Option<&'static str> {
    let list = popular(ecosystem);
    let lower = name.to_ascii_lowercase();
    if list.iter().any(|p| p.eq_ignore_ascii_case(name)) {
        return None;
    }
    let mine = folded(name);
    // An npm scope swap: `@types-node/x` or `@typess/node`.
    let unscoped = lower.rsplit('/').next().unwrap_or(&lower).to_owned();
    list.iter().copied().find(|popular| {
        let theirs = folded(popular);
        if mine == theirs {
            // Same letters, other separators (`python_dateutil`).
            return ecosystem == Ecosystem::Npm;
        }
        let limit = if theirs.len() <= 5 { 1 } else { 2 };
        let close = theirs.len() >= 4 && distance(&mine, &theirs) <= limit;
        let scoped = popular.contains('/')
            && popular.rsplit('/').next() == Some(unscoped.as_str())
            && lower != popular.to_ascii_lowercase();
        close || scoped
    })
}

// ---------- registry lookups ----------

fn base(ecosystem: Ecosystem) -> String {
    let (var, default) = match ecosystem {
        Ecosystem::Npm => ("SHADOWCODE_REGISTRY_NPM", "https://registry.npmjs.org"),
        Ecosystem::PyPI => ("SHADOWCODE_REGISTRY_PYPI", "https://pypi.org"),
        Ecosystem::Crates => ("SHADOWCODE_REGISTRY_CRATES", "https://crates.io"),
        Ecosystem::Go => ("SHADOWCODE_REGISTRY_GO", "https://proxy.golang.org"),
        Ecosystem::RubyGems => ("SHADOWCODE_REGISTRY_RUBYGEMS", "https://rubygems.org"),
    };
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

/// Days since an RFC 3339 / ISO date (`2024-05-01T12:00:00Z`).
fn age_days(date: &str, now: f64) -> Option<f64> {
    let date = date.get(..10)?;
    let mut parts = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((now / 86_400.0 - days as f64).max(0.0))
}

enum Lookup {
    Found(Option<String>),
    Missing,
    Failed(String),
}

async fn fetch(client: &reqwest::Client, url: &str) -> Lookup {
    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(error) => {
            return Lookup::Failed(if error.is_timeout() {
                "the registry did not answer in time".into()
            } else {
                "the registry could not be reached".into()
            })
        }
    };
    match response.status().as_u16() {
        404 | 410 => return Lookup::Missing,
        200 => {}
        code => return Lookup::Failed(format!("the registry answered {code}")),
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return Lookup::Failed("the answer was cut off".into());
        };
        if body.len() + chunk.len() > MAX_ANSWER_BYTES {
            // A very large record: a long-lived, widely used package.
            return Lookup::Found(None);
        }
        body.extend_from_slice(&chunk);
    }
    Lookup::Found(Some(String::from_utf8_lossy(&body).into_owned()))
}

/// The date a package was first published, from its registry record.
fn first_published(ecosystem: Ecosystem, record: &str) -> Option<String> {
    let value: Value = serde_json::from_str(record).ok()?;
    match ecosystem {
        Ecosystem::Npm => value["time"]["created"].as_str().map(str::to_owned),
        Ecosystem::PyPI => value["releases"]
            .as_object()?
            .values()
            .flat_map(|files| files.as_array().cloned().unwrap_or_default())
            .filter_map(|f| f["upload_time_iso_8601"].as_str().map(str::to_owned))
            .min(),
        Ecosystem::Crates => value["crate"]["created_at"].as_str().map(str::to_owned),
        _ => None,
    }
}

async fn look_up(client: &reqwest::Client, package: &Package, now: f64) -> Status {
    let base = base(package.ecosystem);
    let url = match package.ecosystem {
        Ecosystem::Npm => format!("{base}/{}", package.name.replace('/', "%2F")),
        Ecosystem::PyPI => format!("{base}/pypi/{}/json", package.name),
        Ecosystem::Crates => format!("{base}/api/v1/crates/{}", package.name),
        _ => {
            return Status::Unchecked(format!(
                "{} packages aren't checked",
                package.ecosystem.label()
            ))
        }
    };
    match fetch(client, &url).await {
        Lookup::Missing => Status::Missing,
        Lookup::Failed(reason) => Status::Unchecked(reason),
        Lookup::Found(None) => Status::Known { age_days: None },
        Lookup::Found(Some(record)) => {
            let age = first_published(package.ecosystem, &record).and_then(|d| age_days(&d, now));
            match age {
                Some(age) if age < NEW_DAYS => Status::New { age_days: age },
                age => Status::Known { age_days: age },
            }
        }
    }
}

fn cache_key(package: &Package) -> String {
    format!("package_check:{}:{}", package.ecosystem.id(), package.name)
}

fn cached(store: &Store, package: &Package, now: f64) -> Option<Status> {
    let value: Value = serde_json::from_str(&store.native_meta(&cache_key(package)).ok()??).ok()?;
    if now - value["checked_at"].as_f64()? > CACHE_SECONDS {
        return None;
    }
    Some(match value["status"].as_str()? {
        "missing" => Status::Missing,
        "new" => Status::New {
            age_days: value["age_days"].as_f64()?
                + (now - value["checked_at"].as_f64()?) / 86_400.0,
        },
        _ => Status::Known {
            age_days: value["age_days"].as_f64(),
        },
    })
}

fn remember(store: &Store, package: &Package, status: &Status, now: f64) {
    let (kind, age) = match status {
        Status::Known { age_days } => ("known", *age_days),
        Status::New { age_days } => ("new", Some(*age_days)),
        Status::Missing => ("missing", None),
        Status::Unchecked(_) => return,
    };
    let _ = store.set_native_meta(
        &cache_key(package),
        &json!({"status":kind,"age_days":age,"checked_at":now}).to_string(),
    );
}

/// Look every package up (at most 12, a few seconds in all). `offline`
/// skips the network: those packages are "not checked".
pub async fn check(packages: &[Package], offline: bool, store: Option<&Store>) -> Vec<Verdict> {
    let now = crate::now();
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .user_agent(concat!(
            "ShadowCode/",
            env!("CARGO_PKG_VERSION"),
            " (package check)"
        ))
        .build()
        .ok();
    let lookups = packages.iter().take(MAX_LOOKUPS).map(|package| {
        let client = client.clone();
        async move {
            let status = if let Some(status) = store.and_then(|s| cached(s, package, now)) {
                status
            } else if offline {
                Status::Unchecked("offline".into())
            } else if let Some(client) = client {
                let status = look_up(&client, package, now).await;
                if let Some(store) = store {
                    remember(store, package, &status, now);
                }
                status
            } else {
                Status::Unchecked("no network client".into())
            };
            Verdict {
                package: package.clone(),
                lookalike: lookalike(package.ecosystem, &package.name).map(str::to_owned),
                status,
            }
        }
    });
    let mut verdicts = futures_util::future::join_all(lookups).await;
    for package in packages.iter().skip(MAX_LOOKUPS) {
        verdicts.push(Verdict {
            package: package.clone(),
            status: Status::Unchecked("too many packages at once".into()),
            lookalike: lookalike(package.ecosystem, &package.name).map(str::to_owned),
        });
    }
    verdicts
}

fn age_words(days: f64) -> String {
    match days {
        d if d < 1.0 => "today".into(),
        d if d < 2.0 => "yesterday".into(),
        d if d < 60.0 => format!("{} days ago", d.floor()),
        d if d < 730.0 => format!("{} months ago", (d / 30.4).floor()),
        d => format!("{} years ago", (d / 365.25).floor()),
    }
}

/// The approval card's "New packages" section.
pub fn section(verdicts: &[Verdict]) -> Option<Value> {
    if verdicts.is_empty() {
        return None;
    }
    let mut level = "info";
    let mut items = Vec::new();
    for verdict in verdicts {
        let name = format!(
            "{} ({})",
            verdict.package.name,
            verdict.package.ecosystem.label()
        );
        let mut line = match &verdict.status {
            Status::Missing => {
                level = "danger";
                format!(
                    "{name}: not found on {} — check the name",
                    verdict.package.ecosystem.label()
                )
            }
            Status::New { age_days } => {
                if level == "info" {
                    level = "warn";
                }
                format!("{name}: first published {}", age_words(*age_days))
            }
            Status::Known {
                age_days: Some(age),
            } => format!("{name}: published {}", age_words(*age)),
            Status::Known { age_days: None } => format!("{name}: a long-established package"),
            Status::Unchecked(reason) => format!("{name}: not checked ({reason})"),
        };
        if let Some(popular) = &verdict.lookalike {
            level = "danger";
            line.push_str(&format!(
                ". The name looks like the popular “{popular}”: make sure it's the one you want"
            ));
        }
        items.push(line);
    }
    Some(json!({
        "title": if verdicts.len() == 1 { "New package" } else { "New packages" },
        "level": level,
        "items": items,
    }))
}

// ---------- lockfiles ----------

/// Packages and their versions in a lockfile, by its name.
fn lock_entries(path: &str, text: &str) -> Option<BTreeMap<String, BTreeSet<String>>> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut entries: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut add = |name: &str, version: &str| {
        if !name.is_empty() {
            entries
                .entry(name.to_owned())
                .or_default()
                .insert(version.to_owned());
        }
    };
    match name {
        "package-lock.json" | "npm-shrinkwrap.json" => {
            let value: Value = serde_json::from_str(text).ok()?;
            if let Some(packages) = value["packages"].as_object() {
                for (key, info) in packages {
                    if let Some(name) = key
                        .rsplit("node_modules/")
                        .next()
                        .filter(|_| !key.is_empty())
                    {
                        add(name, info["version"].as_str().unwrap_or(""));
                    }
                }
            } else if let Some(deps) = value["dependencies"].as_object() {
                for (name, info) in deps {
                    add(name, info["version"].as_str().unwrap_or(""));
                }
            }
        }
        "Cargo.lock" | "poetry.lock" | "uv.lock" => {
            let value: toml::Table = text.parse().ok()?;
            for package in value.get("package")?.as_array()? {
                add(
                    package.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                    package
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                );
            }
        }
        "yarn.lock" => {
            let mut current: Vec<String> = Vec::new();
            for line in text.lines() {
                if !line.starts_with(' ') && line.ends_with(':') && !line.starts_with('#') {
                    current = line
                        .trim_end_matches(':')
                        .split(", ")
                        .filter_map(|spec| {
                            let spec = spec.trim_matches('"');
                            let at = spec[1..].find('@').map(|i| i + 1)?;
                            Some(spec[..at].to_owned())
                        })
                        .collect();
                } else if let Some(version) = line.trim().strip_prefix("version ") {
                    for name in current.drain(..) {
                        add(&name, version.trim_matches('"'));
                    }
                }
            }
        }
        "pnpm-lock.yaml" => {
            let mut in_packages = false;
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if !line.starts_with(' ') {
                    in_packages = line.trim_end() == "packages:";
                    continue;
                }
                let Some(key) = line.strip_prefix("  ").filter(|l| !l.starts_with(' ')) else {
                    continue;
                };
                if !in_packages {
                    continue;
                }
                let key = key
                    .trim_end_matches(':')
                    .trim_matches('\'')
                    .trim_start_matches('/');
                if let Some(at) = key.get(1..).and_then(|k| k.rfind('@')).map(|i| i + 1) {
                    let version = key[at + 1..].split('(').next().unwrap_or("");
                    add(&key[..at], version);
                }
            }
        }
        "go.sum" => {
            for line in text.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(module), Some(version)) = (parts.next(), parts.next()) {
                    add(module, version.trim_end_matches("/go.mod"));
                }
            }
        }
        _ => return None,
    }
    Some(entries)
}

pub fn is_lockfile(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or(path),
        "package-lock.json"
            | "npm-shrinkwrap.json"
            | "Cargo.lock"
            | "poetry.lock"
            | "uv.lock"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "go.sum"
    )
}

/// What a lockfile change does, in counts and names: `{added, updated,
/// removed}` lists of `{name, from?, to?}`.
pub fn lockfile_summary(path: &str, before: &str, after: &str) -> Option<Value> {
    let old = lock_entries(path, before).unwrap_or_default();
    let new = lock_entries(path, after)?;
    let join = |set: &BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>().join(", ");
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut removed = Vec::new();
    for (name, versions) in &new {
        match old.get(name) {
            None => added.push(json!({"name":name,"to":join(versions)})),
            Some(previous) if previous != versions => {
                updated.push(json!({"name":name,"from":join(previous),"to":join(versions)}))
            }
            _ => {}
        }
    }
    for (name, versions) in &old {
        if !new.contains_key(name) {
            removed.push(json!({"name":name,"from":join(versions)}));
        }
    }
    Some(json!({
        "added": added,
        "updated": updated,
        "removed": removed,
        "summary": format!("{} added, {} updated, {} removed", added.len(), updated.len(), removed.len()),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(packages: Vec<Package>) -> Vec<String> {
        packages
            .into_iter()
            .map(|p| format!("{}:{}", p.ecosystem.label(), p.name))
            .collect()
    }

    #[test]
    fn install_commands_name_their_packages() {
        assert_eq!(
            names(from_command(
                "npm install left-pad@1.3.0 @types/node --save-dev"
            )),
            ["npm:@types/node", "npm:left-pad"]
        );
        assert_eq!(names(from_command("pnpm add -D vitest")), ["npm:vitest"]);
        assert_eq!(names(from_command("yarn add react@^18")), ["npm:react"]);
        assert_eq!(
            names(from_command(
                "pip install 'requests[socks]>=2.31' -r requirements.txt"
            )),
            ["PyPI:requests"]
        );
        assert_eq!(
            names(from_command("python3 -m pip install flask==3.0")),
            ["PyPI:flask"]
        );
        assert_eq!(
            names(from_command("uv add httpx && uv pip install rich")),
            ["PyPI:httpx", "PyPI:rich"]
        );
        assert_eq!(
            names(from_command("cargo add serde --features derive")),
            ["crates.io:serde"]
        );
        assert_eq!(
            names(from_command("go get github.com/spf13/cobra@v1.8.0")),
            ["Go:github.com/spf13/cobra"]
        );
        assert_eq!(
            names(from_command("sudo gem install rails")),
            ["RubyGems:rails"]
        );
        for none in [
            "npm install",
            "npm ci",
            "npm install ./local-pkg",
            "pip install -e .",
            "cargo build",
            "pip install -r requirements.txt",
            "npm test",
        ] {
            assert!(from_command(none).is_empty(), "{none}");
        }
    }

    #[test]
    fn manifest_edits_name_only_the_added_dependencies() {
        let before = r#"{"name":"app","scripts":{"build":"tsc"},"dependencies":{"react":"^18"}}"#;
        let after = r#"{"name":"app","scripts":{"build":"tsc","lint":"eslint ."},"dependencies":{"react":"^18","lodahs":"^4"},"devDependencies":{"local":"file:../local"}}"#;
        assert_eq!(
            names(from_manifest("web/package.json", Some(before), after)),
            ["npm:lodahs"]
        );
        let cargo = "[package]\nname = \"x\"\n[dependencies]\nserde = \"1\"\nmine = { path = \"../mine\" }\n";
        let cargo_after = format!("{cargo}tokio = {{ version = \"1\", features = [\"full\"] }}\n");
        assert_eq!(
            names(from_manifest("Cargo.toml", Some(cargo), &cargo_after)),
            ["crates.io:tokio"]
        );
        assert_eq!(
            names(from_manifest(
                "requirements-dev.txt",
                Some("pytest\n"),
                "pytest\nreqeusts==2\n# comment\n-e .\n"
            )),
            ["PyPI:reqeusts"]
        );
        let py = "[project]\nname = \"x\"\ndependencies = [\"httpx>=0.27\"]\n";
        let py_after =
            "[project]\nname = \"x\"\ndependencies = [\"httpx>=0.27\", \"pydantic>=2\"]\n";
        assert_eq!(
            names(from_manifest("pyproject.toml", Some(py), py_after)),
            ["PyPI:pydantic"]
        );
        let go = "module x\n\ngo 1.22\n\nrequire (\n\tgithub.com/a/b v1.0.0\n)\n";
        let go_after = "module x\n\ngo 1.22\n\nrequire (\n\tgithub.com/a/b v1.0.0\n\tgithub.com/c/d v0.1.0\n)\n";
        assert_eq!(
            names(from_manifest("go.mod", Some(go), go_after)),
            ["Go:github.com/c/d"]
        );
        assert!(from_manifest("README.md", None, "anything").is_empty());
    }

    #[test]
    fn lookalikes_of_popular_names() {
        assert_eq!(lookalike(Ecosystem::Npm, "lodahs"), Some("lodash"));
        assert_eq!(lookalike(Ecosystem::Npm, "reqeust"), Some("request"));
        assert_eq!(lookalike(Ecosystem::Npm, "cross_env"), Some("cross-env"));
        assert_eq!(lookalike(Ecosystem::PyPI, "reqeusts"), Some("requests"));
        assert_eq!(
            lookalike(Ecosystem::PyPI, "python-dateutils"),
            Some("python-dateutil")
        );
        assert_eq!(
            lookalike(Ecosystem::Crates, "serde-json"),
            None,
            "separators are the same crate name"
        );
        assert_eq!(lookalike(Ecosystem::Crates, "tokoi"), Some("tokio"));
        for fine in [
            "lodash",
            "react",
            "zod",
            "left-pad",
            "my-internal-tool",
            "serde_json",
        ] {
            assert_eq!(lookalike(Ecosystem::Npm, fine), None, "{fine}");
        }
        assert_eq!(
            lookalike(Ecosystem::Npm, "@typess/node"),
            Some("@types/node")
        );
        assert_eq!(lookalike(Ecosystem::Npm, "@evil/node"), Some("@types/node"));
    }

    #[test]
    fn ages_and_sections() {
        let at = 1_790_000_000.0; // 2026-09-21
        assert!((age_days("2026-09-01T10:00:00Z", at).unwrap() - 20.0).abs() < 2.0);
        assert!(age_days("2010-01-01", at).unwrap() > 5_000.0);
        let verdicts = vec![
            Verdict {
                package: Package {
                    ecosystem: Ecosystem::Npm,
                    name: "left-pad".into(),
                },
                status: Status::Known {
                    age_days: Some(4_000.0),
                },
                lookalike: None,
            },
            Verdict {
                package: Package {
                    ecosystem: Ecosystem::Npm,
                    name: "fresh-thing".into(),
                },
                status: Status::New { age_days: 3.0 },
                lookalike: None,
            },
        ];
        let section = section(&verdicts).unwrap();
        assert_eq!(section["level"], "warn");
        assert_eq!(
            section["items"][0],
            "left-pad (npm): published 10 years ago"
        );
        assert_eq!(
            section["items"][1],
            "fresh-thing (npm): first published 3 days ago"
        );
        let missing = vec![Verdict {
            package: Package {
                ecosystem: Ecosystem::PyPI,
                name: "reqeusts".into(),
            },
            status: Status::Missing,
            lookalike: Some("requests".into()),
        }];
        let section = super::section(&missing).unwrap();
        assert_eq!(section["level"], "danger");
        assert!(section["items"][0]
            .as_str()
            .unwrap()
            .contains("not found on PyPI"));
        assert!(section["items"][0].as_str().unwrap().contains("“requests”"));
    }

    #[test]
    fn lockfile_changes_are_summarized() {
        let before = r#"{"lockfileVersion":3,"packages":{"":{"name":"app"},"node_modules/react":{"version":"18.2.0"},"node_modules/old":{"version":"1.0.0"}}}"#;
        let after = r#"{"lockfileVersion":3,"packages":{"":{"name":"app"},"node_modules/react":{"version":"18.3.1"},"node_modules/zod":{"version":"3.23.8"},"node_modules/a/node_modules/zod":{"version":"3.22.0"}}}"#;
        let summary = lockfile_summary("package-lock.json", before, after).unwrap();
        assert_eq!(summary["summary"], "1 added, 1 updated, 1 removed");
        assert_eq!(summary["added"][0]["name"], "zod");
        assert_eq!(summary["added"][0]["to"], "3.22.0, 3.23.8");
        assert_eq!(summary["updated"][0]["from"], "18.2.0");
        let cargo_before = "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.1\"\n";
        let cargo_after = "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.2\"\n\n[[package]]\nname = \"tokio\"\nversion = \"1.40.0\"\n";
        assert_eq!(
            lockfile_summary("Cargo.lock", cargo_before, cargo_after).unwrap()["summary"],
            "1 added, 1 updated, 0 removed"
        );
        let yarn = "\"react@^18.0.0\":\n  version \"18.2.0\"\n\nzod@^3:\n  version \"3.23.8\"\n";
        assert_eq!(
            lockfile_summary("yarn.lock", "", yarn).unwrap()["summary"],
            "2 added, 0 updated, 0 removed"
        );
        let pnpm = "lockfileVersion: '9.0'\n\npackages:\n\n  react@18.3.1:\n    resolution: {}\n\n  '@types/node@20.1.0':\n    resolution: {}\n";
        let summary = lockfile_summary("pnpm-lock.yaml", "", pnpm).unwrap();
        assert_eq!(
            summary["summary"], "2 added, 0 updated, 0 removed",
            "{summary}"
        );
        assert!(is_lockfile("web/pnpm-lock.yaml"));
        assert!(!is_lockfile("package.json"));
    }
}
