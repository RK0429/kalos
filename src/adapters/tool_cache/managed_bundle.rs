use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use tar::Archive;
use thiserror::Error;

use crate::ports::tool_cache::{ResolvedToolBundle, ToolCachePort, ToolCacheRequest};

pub const BUNDLE_MARKER_FILE: &str = "bundle.marker";
const BUNDLE_BOOTSTRAP_FAILURE_FILE: &str = "bootstrap.failure";

/// Kalos CPG extraction queries embedded in the binary.
///
/// These are deployed to the CodeQL bundle cache alongside the CLI so that
/// `codeql query run` can find them. Each query emits named predicates that
/// decode into the JSON shape expected by `CpgNormalizer`.
const BUNDLED_QUERIES: &[(&str, &str)] = &[
    (
        "extract-python.ql",
        r#"import python
import semmle.python.objects.ObjectAPI

private string moduleId(Module m) { result = "mod_" + m.getFile().getRelativePath() }

private string functionId(Function f) {
  result =
    "fn_" + f.getLocation().getFile().getRelativePath() + ":" +
      f.getLocation().getStartLine().toString() + ":" + f.getQualifiedName()
}

private string classId(Class c) {
  result =
    "cls_" + c.getLocation().getFile().getRelativePath() + ":" +
      c.getLocation().getStartLine().toString() + ":" + c.getQualifiedName()
}

private string parameterId(Parameter p) {
  result =
    "param_" + p.getLocation().getFile().getRelativePath() + ":" +
      p.getLocation().getStartLine().toString() + ":" + p.asName().getId()
}

private string variableId(LocalVariable v) {
  result =
    "var_" + v.getAnAccess().getLocation().getFile().getRelativePath() + ":" +
      v.getAnAccess().getLocation().getStartLine().toString() + ":" + v.getId()
}

private predicate functionParameter(Function f, Parameter p) {
  f.getAnArg() = p
  or
  f.getAKeywordOnlyArg() = p
  or
  f.getVararg() = p
  or
  f.getKwarg() = p
}

query predicate modules(string id, string name, string file, int start_line, int end_line) {
  exists(Module m |
    id = moduleId(m) and
    name = m.getFile().getRelativePath() and
    file = m.getFile().getRelativePath() and
    start_line = 1 and
    end_line = 1
  )
}

query predicate classes(string id, string name, string file, int start_line, int end_line) {
  exists(Class c |
    exists(c.getQualifiedName()) and
    id = classId(c) and
    name = c.getName() and
    file = c.getLocation().getFile().getRelativePath() and
    start_line = c.getLocation().getStartLine() and
    end_line = c.getLocation().getEndLine()
  )
}

query predicate functions(string id, string name, string file, int start_line, int end_line) {
  exists(Function f |
    exists(f.getQualifiedName()) and
    id = functionId(f) and
    name = f.getName() and
    file = f.getLocation().getFile().getRelativePath() and
    start_line = f.getLocation().getStartLine() and
    end_line = f.getLocation().getEndLine()
  )
}

query predicate parameters(string id, string name, string file, int start_line, int end_line) {
  exists(Function f, Parameter p |
    functionParameter(f, p) and
    exists(p.asName()) and
    id = parameterId(p) and
    name = p.asName().getId() and
    file = p.getLocation().getFile().getRelativePath() and
    start_line = p.getLocation().getStartLine() and
    end_line = p.getLocation().getEndLine()
  )
}

query predicate variables(string id, string name, string file, int start_line, int end_line) {
  exists(Function f, LocalVariable v |
    v.getScope() = f and
    not v.isParameter() and
    id = variableId(v) and
    name = v.getId() and
    file = v.getAnAccess().getLocation().getFile().getRelativePath() and
    start_line = v.getAnAccess().getLocation().getStartLine() and
    end_line = v.getAnAccess().getLocation().getEndLine()
  )
}

query predicate contains(string source, string target) {
  exists(Module m, Function f |
    f.getScope() = m and
    source = moduleId(m) and
    target = functionId(f)
  )
  or
  exists(Module m, Class c |
    c.getScope() = m and
    source = moduleId(m) and
    target = classId(c)
  )
  or
  exists(Function f, Parameter p |
    functionParameter(f, p) and
    exists(p.asName()) and
    source = functionId(f) and
    target = parameterId(p)
  )
  or
  exists(Function f, LocalVariable v |
    v.getScope() = f and
    not v.isParameter() and
    source = functionId(f) and
    target = variableId(v)
  )
}

query predicate calls(string source, string target) {
  exists(Call call, Function caller, FunctionValue callee_value, Function callee |
    call.getScope() = caller and
    callee_value.getACall() = call.getAFlowNode() and
    callee_value.getScope() = callee and
    source = functionId(caller) and
    target = functionId(callee)
  )
}

query predicate control_flows(string source, string target) {
  exists(Function f, Parameter p |
    functionParameter(f, p) and
    exists(p.asName()) and
    source = functionId(f) and
    target = parameterId(p)
  )
  or
  exists(Function f, LocalVariable v |
    v.getScope() = f and
    not v.isParameter() and
    source = functionId(f) and
    target = variableId(v)
  )
}

query predicate data_flows(string source, string target) {
  exists(Function f, LocalVariable src, LocalVariable dst |
    src.getScope() = f and
    dst.getScope() = f and
    src != dst and
    source = variableId(src) and
    target = variableId(dst)
  )
  or
  exists(Function f, Parameter src, LocalVariable dst |
    functionParameter(f, src) and
    dst.getScope() = f and
    exists(src.asName()) and
    source = parameterId(src) and
    target = variableId(dst)
  )
}
"#,
    ),
    (
        "extract-javascript-typescript.ql",
        r#"import javascript

private string moduleId(TopLevel tl) { result = "mod_" + tl.getFile().getRelativePath() }

private string functionId(Function f) {
  result =
    "fn_" + f.getLocation().getFile().getRelativePath() + ":" +
      f.getLocation().getStartLine().toString() + ":" + f.getName()
}

private string classId(ClassDefinition c) {
  result =
    "cls_" + c.getLocation().getFile().getRelativePath() + ":" +
      c.getLocation().getStartLine().toString() + ":" + c.getName()
}

private string parameterId(Parameter p) {
  result =
    "param_" + p.getLocation().getFile().getRelativePath() + ":" +
      p.getLocation().getStartLine().toString() + ":" + p.getName()
}

private string variableId(LocalVariable v) {
  result =
    "var_" + v.getLocation().getFile().getRelativePath() + ":" +
      v.getLocation().getStartLine().toString() + ":" + v.getName()
}

query predicate modules(string id, string name, string file, int start_line, int end_line) {
  exists(TopLevel tl |
    id = moduleId(tl) and
    name = tl.getFile().getRelativePath() and
    file = tl.getFile().getRelativePath() and
    start_line = tl.getLocation().getStartLine() and
    end_line = tl.getLocation().getEndLine()
  )
}

query predicate classes(string id, string name, string file, int start_line, int end_line) {
  exists(ClassDefinition c |
    exists(c.getName()) and
    id = classId(c) and
    name = c.getName() and
    file = c.getLocation().getFile().getRelativePath() and
    start_line = c.getLocation().getStartLine() and
    end_line = c.getLocation().getEndLine()
  )
}

query predicate functions(string id, string name, string file, int start_line, int end_line) {
  exists(Function f |
    exists(f.getName()) and
    id = functionId(f) and
    name = f.getName() and
    file = f.getLocation().getFile().getRelativePath() and
    start_line = f.getLocation().getStartLine() and
    end_line = f.getLocation().getEndLine()
  )
}

query predicate parameters(string id, string name, string file, int start_line, int end_line) {
  exists(Function f, Parameter p |
    p = f.getAParameter() and
    id = parameterId(p) and
    name = p.getName() and
    file = p.getLocation().getFile().getRelativePath() and
    start_line = p.getLocation().getStartLine() and
    end_line = p.getLocation().getEndLine()
  )
}

query predicate variables(string id, string name, string file, int start_line, int end_line) {
  exists(Function f, LocalVariable v |
    v.getDeclaringContainer() = f and
    not v.isParameter() and
    id = variableId(v) and
    name = v.getName() and
    file = v.getLocation().getFile().getRelativePath() and
    start_line = v.getLocation().getStartLine() and
    end_line = v.getLocation().getEndLine()
  )
}

query predicate contains(string source, string target) {
  exists(TopLevel tl, Function f |
    f.getTopLevel() = tl and
    exists(f.getName()) and
    source = moduleId(tl) and
    target = functionId(f)
  )
  or
  exists(TopLevel tl, ClassDefinition c |
    c.getTopLevel() = tl and
    exists(c.getName()) and
    source = moduleId(tl) and
    target = classId(c)
  )
  or
  exists(Function f, Parameter p |
    p = f.getAParameter() and
    source = functionId(f) and
    target = parameterId(p)
  )
  or
  exists(Function f, LocalVariable v |
    v.getDeclaringContainer() = f and
    not v.isParameter() and
    source = functionId(f) and
    target = variableId(v)
  )
}

query predicate calls(string source, string target) {
  exists(CallExpr call, Function caller, Function callee |
    call.getEnclosingFunction() = caller and
    callee = call.getResolvedCallee() and
    exists(caller.getName()) and
    exists(callee.getName()) and
    source = functionId(caller) and
    target = functionId(callee)
  )
}

query predicate control_flows(string source, string target) {
  exists(Function f, Parameter p |
    p = f.getAParameter() and
    source = functionId(f) and
    target = parameterId(p)
  )
  or
  exists(Function f, LocalVariable v |
    v.getDeclaringContainer() = f and
    not v.isParameter() and
    source = functionId(f) and
    target = variableId(v)
  )
}

query predicate data_flows(string source, string target) {
  exists(Function f, Parameter src, LocalVariable dst |
    src = f.getAParameter() and
    dst.getDeclaringContainer() = f and
    not dst.isParameter() and
    source = parameterId(src) and
    target = variableId(dst)
  )
  or
  exists(Function f, LocalVariable src, LocalVariable dst |
    src.getDeclaringContainer() = f and
    dst.getDeclaringContainer() = f and
    not src.isParameter() and
    not dst.isParameter() and
    src != dst and
    source = variableId(src) and
    target = variableId(dst)
  )
}
"#,
    ),
    (
        "extract-rust.ql",
        r#"import rust
import codeql.files.FileSystem

private string moduleId(File f) { result = "mod_" + f.getRelativePath() }

private string functionId(Function f) {
  result =
    "fn_" + f.getFile().getRelativePath() + ":" + f.getLocation().getStartLine().toString() +
      ":" + f.getName().getText()
}

private string structId(Struct s) {
  result =
    "cls_" + s.getFile().getRelativePath() + ":" + s.getLocation().getStartLine().toString() +
      ":" + s.getName().getText()
}

private string enumId(Enum e) {
  result =
    "cls_" + e.getFile().getRelativePath() + ":" + e.getLocation().getStartLine().toString() +
      ":" + e.getName().getText()
}

private string traitId(Trait t) {
  result =
    "cls_" + t.getFile().getRelativePath() + ":" + t.getLocation().getStartLine().toString() +
      ":" + t.getName().getText()
}

private string parameterId(Param p) {
  result =
    "param_" + p.getFile().getRelativePath() + ":" + p.getLocation().getStartLine().toString() +
      ":" + p.getPat().toString()
}

private string variableId(LetStmt l) {
  result =
    "var_" + l.getFile().getRelativePath() + ":" + l.getLocation().getStartLine().toString() +
      ":" + l.getPat().toString()
}

query predicate modules(string id, string name, string file, int start_line, int end_line) {
  exists(File f |
    (
      exists(Function func | func.fromSource() and func.getFile() = f)
      or
      exists(Struct s | s.fromSource() and s.getFile() = f)
      or
      exists(Enum e | e.fromSource() and e.getFile() = f)
      or
      exists(Trait t | t.fromSource() and t.getFile() = f)
    ) and
    id = moduleId(f) and
    name = f.getRelativePath() and
    file = f.getRelativePath() and
    start_line = 1 and
    end_line = 1
  )
}

query predicate classes(string id, string name, string file, int start_line, int end_line) {
  exists(Struct s |
    s.fromSource() and
    id = structId(s) and
    name = s.getName().getText() and
    file = s.getFile().getRelativePath() and
    start_line = s.getLocation().getStartLine() and
    end_line = s.getLocation().getEndLine()
  )
  or
  exists(Enum e |
    e.fromSource() and
    id = enumId(e) and
    name = e.getName().getText() and
    file = e.getFile().getRelativePath() and
    start_line = e.getLocation().getStartLine() and
    end_line = e.getLocation().getEndLine()
  )
  or
  exists(Trait t |
    t.fromSource() and
    id = traitId(t) and
    name = t.getName().getText() and
    file = t.getFile().getRelativePath() and
    start_line = t.getLocation().getStartLine() and
    end_line = t.getLocation().getEndLine()
  )
}

query predicate functions(string id, string name, string file, int start_line, int end_line) {
  exists(Function f |
    f.fromSource() and
    id = functionId(f) and
    name = f.getName().getText() and
    file = f.getFile().getRelativePath() and
    start_line = f.getLocation().getStartLine() and
    end_line = f.getLocation().getEndLine()
  )
}

query predicate parameters(string id, string name, string file, int start_line, int end_line) {
  exists(Function f, Param p |
    p.getEnclosingCallable() = f and
    f.fromSource() and
    id = parameterId(p) and
    name = p.getPat().toString() and
    file = p.getFile().getRelativePath() and
    start_line = p.getLocation().getStartLine() and
    end_line = p.getLocation().getEndLine()
  )
}

query predicate variables(string id, string name, string file, int start_line, int end_line) {
  exists(Function f, LetStmt l |
    l.getEnclosingCallable() = f and
    f.fromSource() and
    id = variableId(l) and
    name = l.getPat().toString() and
    file = l.getFile().getRelativePath() and
    start_line = l.getLocation().getStartLine() and
    end_line = l.getLocation().getEndLine()
  )
}

query predicate contains(string source, string target) {
  exists(Function f |
    f.fromSource() and
    source = moduleId(f.getFile()) and
    target = functionId(f)
  )
  or
  exists(Struct s |
    s.fromSource() and
    source = moduleId(s.getFile()) and
    target = structId(s)
  )
  or
  exists(Enum e |
    e.fromSource() and
    source = moduleId(e.getFile()) and
    target = enumId(e)
  )
  or
  exists(Trait t |
    t.fromSource() and
    source = moduleId(t.getFile()) and
    target = traitId(t)
  )
  or
  exists(Function f, Param p |
    p.getEnclosingCallable() = f and
    f.fromSource() and
    source = functionId(f) and
    target = parameterId(p)
  )
  or
  exists(Function f, LetStmt l |
    l.getEnclosingCallable() = f and
    f.fromSource() and
    source = functionId(f) and
    target = variableId(l)
  )
}

query predicate calls(string source, string target) {
  exists(Call call, Function caller, Function callee |
    caller = call.getEnclosingCallable() and
    callee = call.getStaticTarget() and
    caller.fromSource() and
    callee.fromSource() and
    source = functionId(caller) and
    target = functionId(callee)
  )
}

query predicate control_flows(string source, string target) {
  exists(Function f, Param p |
    p.getEnclosingCallable() = f and
    f.fromSource() and
    source = functionId(f) and
    target = parameterId(p)
  )
  or
  exists(Function f, LetStmt l |
    l.getEnclosingCallable() = f and
    f.fromSource() and
    source = functionId(f) and
    target = variableId(l)
  )
}

query predicate data_flows(string source, string target) {
  exists(Function f, Param src, LetStmt dst |
    src.getEnclosingCallable() = f and
    dst.getEnclosingCallable() = f and
    f.fromSource() and
    source = parameterId(src) and
    target = variableId(dst)
  )
  or
  exists(Function f, LetStmt src, LetStmt dst |
    src.getEnclosingCallable() = f and
    dst.getEnclosingCallable() = f and
    src != dst and
    f.fromSource() and
    source = variableId(src) and
    target = variableId(dst)
  )
}
"#,
    ),
    (
        "extract-go.ql",
        r#"import go

private string functionId(FuncDecl fd) {
  result =
    "fn_" + fd.getFile().getRelativePath() + ":" +
      fd.getLocation().getStartLine().toString() + ":" + fd.getName()
}

private string classId(TypeSpec t) {
  result =
    "cls_" + t.getFile().getRelativePath() + ":" +
      t.getLocation().getStartLine().toString() + ":" + t.getName()
}

private string parameterId(Parameter p) {
  result =
    "param_" + p.getLocation().getFile().getRelativePath() + ":" +
      p.getLocation().getStartLine().toString() + ":" + p.getName()
}

private string variableId(LocalVariable v) {
  result =
    "var_" + v.getLocation().getFile().getRelativePath() + ":" +
      v.getLocation().getStartLine().toString() + ":" + v.getName()
}

query predicate modules(string id, string name, string file, int start_line, int end_line) {
  exists(FuncDecl fd |
    id = "mod_" + fd.getFile().getRelativePath() and
    name = fd.getFile().getRelativePath() and
    file = fd.getFile().getRelativePath() and
    start_line = 1 and
    end_line = 1
  )
  or
  exists(TypeSpec t |
    id = "mod_" + t.getFile().getRelativePath() and
    name = t.getFile().getRelativePath() and
    file = t.getFile().getRelativePath() and
    start_line = 1 and
    end_line = 1
  )
}

query predicate classes(string id, string name, string file, int start_line, int end_line) {
  exists(TypeSpec t |
    id = classId(t) and
    name = t.getName() and
    file = t.getFile().getRelativePath() and
    start_line = t.getLocation().getStartLine() and
    end_line = t.getLocation().getEndLine()
  )
}

query predicate functions(string id, string name, string file, int start_line, int end_line) {
  exists(FuncDecl fd |
    id = functionId(fd) and
    name = fd.getName() and
    file = fd.getFile().getRelativePath() and
    start_line = fd.getLocation().getStartLine() and
    end_line = fd.getLocation().getEndLine()
  )
}

query predicate parameters(string id, string name, string file, int start_line, int end_line) {
  exists(FuncDecl fd, Parameter p |
    p = fd.getAParameter() and
    id = parameterId(p) and
    name = p.getName() and
    file = p.getLocation().getFile().getRelativePath() and
    start_line = p.getLocation().getStartLine() and
    end_line = p.getLocation().getEndLine()
  )
}

query predicate variables(string id, string name, string file, int start_line, int end_line) {
  exists(FuncDecl fd, LocalVariable v |
    v.getDeclaringFunction() = fd and
    id = variableId(v) and
    name = v.getName() and
    file = v.getLocation().getFile().getRelativePath() and
    start_line = v.getLocation().getStartLine() and
    end_line = v.getLocation().getEndLine()
  )
}

query predicate contains(string source, string target) {
  exists(FuncDecl fd |
    source = "mod_" + fd.getFile().getRelativePath() and
    target = functionId(fd)
  )
  or
  exists(TypeSpec t |
    source = "mod_" + t.getFile().getRelativePath() and
    target = classId(t)
  )
  or
  exists(FuncDecl fd, Parameter p |
    p = fd.getAParameter() and
    source = functionId(fd) and
    target = parameterId(p)
  )
  or
  exists(FuncDecl fd, LocalVariable v |
    v.getDeclaringFunction() = fd and
    source = functionId(fd) and
    target = variableId(v)
  )
}

query predicate calls(string source, string target) {
  exists(CallExpr call, FuncDecl caller, FuncDecl callee |
    call.getEnclosingFunction() = caller and
    callee.getFunction() = call.getTarget() and
    source = functionId(caller) and
    target = functionId(callee)
  )
}

query predicate control_flows(string source, string target) {
  exists(FuncDecl fd, Parameter p |
    p = fd.getAParameter() and
    source = functionId(fd) and
    target = parameterId(p)
  )
  or
  exists(FuncDecl fd, LocalVariable v |
    v.getDeclaringFunction() = fd and
    source = functionId(fd) and
    target = variableId(v)
  )
}

query predicate data_flows(string source, string target) {
  exists(FuncDecl fd, Parameter src, LocalVariable dst |
    src = fd.getAParameter() and
    dst.getDeclaringFunction() = fd and
    source = parameterId(src) and
    target = variableId(dst)
  )
  or
  exists(FuncDecl fd, LocalVariable src, LocalVariable dst |
    src.getDeclaringFunction() = fd and
    dst.getDeclaringFunction() = fd and
    src != dst and
    source = variableId(src) and
    target = variableId(dst)
  )
}
"#,
    ),
];

/// Per-language qlpack.yml contents deployed alongside extraction queries.
///
/// Each language needs its own qlpack because CodeQL requires a single
/// dbscheme per pack. The pack name and dependency are derived from the
/// language identifier.
const BUNDLED_QLPACKS: &[(&str, &str)] = &[
    (
        "python",
        "name: kalos/extract-python\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\n",
    ),
    (
        "javascript-typescript",
        "name: kalos/extract-js-ts\nversion: 0.0.1\ndependencies:\n  codeql/javascript-all: \"*\"\n",
    ),
    (
        "rust",
        "name: kalos/extract-rust\nversion: 0.0.1\ndependencies:\n  codeql/rust-all: \"*\"\n",
    ),
    (
        "go",
        "name: kalos/extract-go\nversion: 0.0.1\ndependencies:\n  codeql/go-all: \"*\"\n",
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    LinuxX64,
    LinuxArm64,
    MacosX64,
    MacosArm64,
    WindowsX64,
}

impl Platform {
    pub fn detect() -> Option<Self> {
        if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
            Some(Self::LinuxX64)
        } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
            Some(Self::LinuxArm64)
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
            Some(Self::MacosX64)
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
            Some(Self::MacosArm64)
        } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
            Some(Self::WindowsX64)
        } else {
            None
        }
    }

    /// Returns true when the CodeQL bundle uses x86_64 binary on a non-x86_64 host.
    /// On Apple Silicon this means Rosetta 2 emulation; on Linux ARM64 this means
    /// QEMU or similar emulation.
    pub fn is_emulated(&self) -> bool {
        matches!(self, Self::MacosArm64 | Self::LinuxArm64)
    }

    /// Returns a user-facing notice about emulation overhead, if applicable.
    pub fn emulation_notice(&self) -> Option<&'static str> {
        match self {
            Self::MacosArm64 => Some(
                "note: CodeQL does not provide a native ARM64 bundle for macOS. \
                 The x86_64 bundle will run via Rosetta 2, which may be significantly \
                 slower on first invocation.",
            ),
            Self::LinuxArm64 => Some(
                "note: CodeQL does not provide a native aarch64 bundle for Linux. \
                 The x86_64 bundle requires emulation (e.g. QEMU), which may be \
                 significantly slower.",
            ),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleManifest {
    pub version: String,
    pub sha256: String,
    pub download_url: String,
}

pub fn codeql_bundle_manifest() -> Result<BundleManifest, ManagedToolCacheError> {
    let platform = Platform::detect().ok_or(ManagedToolCacheError::UnsupportedPlatform)?;
    let (download_url, sha256) = match platform {
        Platform::LinuxX64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-linux64.tar.gz",
            "6f867b8734a39b55929a1785d3ae843126ec68564f5598807ae8e126a5c35bba",
        ),
        // CodeQL does not publish a native aarch64 Linux bundle; use x86_64 (requires emulation)
        Platform::LinuxArm64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-linux64.tar.gz",
            "6f867b8734a39b55929a1785d3ae843126ec68564f5598807ae8e126a5c35bba",
        ),
        Platform::MacosX64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-osx64.tar.gz",
            "a5e0bc832929c0ba0a93a123abae6111ab4c3fd64a5477485074f2e131e55486",
        ),
        // CodeQL x86_64 bundle runs on Apple Silicon via Rosetta 2
        Platform::MacosArm64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-osx64.tar.gz",
            "a5e0bc832929c0ba0a93a123abae6111ab4c3fd64a5477485074f2e131e55486",
        ),
        Platform::WindowsX64 => (
            "https://github.com/github/codeql-action/releases/download/codeql-bundle-v2.25.1/codeql-bundle-win64.tar.gz",
            "791030c4201d4a35afb7f9efa4b8b667bd5c5902514c8ab1815907f5b158ab43",
        ),
    };

    Ok(BundleManifest {
        version: "2.25.1".to_owned(),
        sha256: sha256.to_owned(),
        download_url: download_url.to_owned(),
    })
}

#[derive(Clone, Debug)]
pub struct ManagedToolCacheAdapter {
    manifest: BundleManifest,
    cache_dir: Option<PathBuf>,
    bundle_setup_timeout: Duration,
}

#[derive(Debug)]
struct BundleBootstrapLock {
    path: PathBuf,
    heartbeat_stop: Arc<AtomicBool>,
    heartbeat_thread: Option<JoinHandle<()>>,
}

const BUNDLE_BOOTSTRAP_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);
const BUNDLE_BOOTSTRAP_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const BUNDLE_BOOTSTRAP_LOCK_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(50);
const BUNDLE_SETUP_TIMEOUT: Duration = Duration::from_secs(540);

impl BundleBootstrapLock {
    fn acquire_before(
        bundle_root: &Path,
        version: &str,
        deadline: Instant,
        setup_timeout: Duration,
    ) -> Result<Self, ManagedToolCacheError> {
        Self::acquire_with_timeout_before(
            bundle_root,
            version,
            BUNDLE_BOOTSTRAP_LOCK_TIMEOUT,
            deadline,
            setup_timeout,
        )
    }

    #[cfg(test)]
    fn acquire_with_timeout(
        bundle_root: &Path,
        version: &str,
        timeout: Duration,
    ) -> Result<Self, ManagedToolCacheError> {
        Self::acquire_with_timeout_before(
            bundle_root,
            version,
            timeout,
            Instant::now() + BUNDLE_SETUP_TIMEOUT,
            BUNDLE_SETUP_TIMEOUT,
        )
    }

    fn acquire_with_timeout_before(
        bundle_root: &Path,
        version: &str,
        timeout: Duration,
        deadline: Instant,
        setup_timeout: Duration,
    ) -> Result<Self, ManagedToolCacheError> {
        let lock_path = bundle_root.join(format!(".codeql-bundle-{version}.lock.d"));
        loop {
            match fs::create_dir(&lock_path) {
                Ok(()) => {
                    if let Err(source) = fs::write(
                        lock_path.join("owner"),
                        format!("pid={}\n", std::process::id()),
                    ) {
                        let _ = fs::remove_dir_all(&lock_path);
                        return Err(bootstrap_extract_error(
                            version.to_owned(),
                            Some(bundle_root),
                            source,
                        ));
                    }

                    write_lock_heartbeat(&lock_path).map_err(|source| {
                        let _ = fs::remove_dir_all(&lock_path);
                        bootstrap_extract_error(version.to_owned(), Some(bundle_root), source)
                    })?;
                    let heartbeat_stop = Arc::new(AtomicBool::new(false));
                    let heartbeat_thread =
                        start_lock_heartbeat(lock_path.clone(), Arc::clone(&heartbeat_stop));

                    return Ok(Self {
                        path: lock_path,
                        heartbeat_stop,
                        heartbeat_thread: Some(heartbeat_thread),
                    });
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                    match bootstrap_lock_state(&lock_path, timeout) {
                        BootstrapLockState::Idle => {
                            return Err(stale_bootstrap_lock_error(
                                version.to_owned(),
                                bundle_root,
                                &lock_path,
                                timeout,
                            ));
                        }
                        BootstrapLockState::Active | BootstrapLockState::Missing => {}
                    }
                    let remaining = remaining_bundle_setup_timeout(
                        deadline,
                        setup_timeout,
                        "bootstrap lock wait",
                    )
                    .map_err(|source| {
                        bootstrap_lock_wait_timeout_error(
                            version.to_owned(),
                            bundle_root,
                            &lock_path,
                            source,
                        )
                    })?;
                    std::thread::sleep(remaining.min(BUNDLE_BOOTSTRAP_LOCK_POLL_INTERVAL));
                }
                Err(source) => {
                    return Err(bootstrap_extract_error(
                        version.to_owned(),
                        Some(bundle_root),
                        source,
                    ));
                }
            }
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum BootstrapLockState {
    Active,
    Idle,
    Missing,
}

impl Drop for BundleBootstrapLock {
    fn drop(&mut self) {
        self.heartbeat_stop.store(true, Ordering::Relaxed);
        if let Some(heartbeat_thread) = self.heartbeat_thread.take() {
            let _ = heartbeat_thread.join();
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn start_lock_heartbeat(lock_path: PathBuf, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(BUNDLE_BOOTSTRAP_LOCK_HEARTBEAT_INTERVAL);
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let _ = write_lock_heartbeat(&lock_path);
        }
    })
}

fn write_lock_heartbeat(lock_path: &Path) -> io::Result<()> {
    let mut heartbeat = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(lock_path.join("heartbeat"))?;
    writeln!(heartbeat, "pid={}", std::process::id())
}

fn bootstrap_lock_state(lock_path: &Path, timeout: Duration) -> BootstrapLockState {
    match bootstrap_lock_last_progress_at(lock_path) {
        Ok(last_progress_at)
            if SystemTime::now()
                .duration_since(last_progress_at)
                .unwrap_or_default()
                >= timeout =>
        {
            BootstrapLockState::Idle
        }
        Ok(_) => BootstrapLockState::Active,
        Err(error) if error.kind() == io::ErrorKind::NotFound => BootstrapLockState::Missing,
        Err(_) => BootstrapLockState::Idle,
    }
}

fn bootstrap_lock_last_progress_at(lock_path: &Path) -> io::Result<SystemTime> {
    let mut last_progress_at = lock_path.metadata()?.modified()?;
    for name in ["owner", "heartbeat"] {
        let path = lock_path.join(name);
        match path.metadata().and_then(|metadata| metadata.modified()) {
            Ok(modified_at) if modified_at > last_progress_at => {
                last_progress_at = modified_at;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(last_progress_at)
}

impl ManagedToolCacheAdapter {
    pub fn new(manifest: BundleManifest) -> Self {
        Self {
            manifest,
            cache_dir: None,
            bundle_setup_timeout: BUNDLE_SETUP_TIMEOUT,
        }
    }

    pub fn with_cache_dir(manifest: BundleManifest, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            manifest,
            cache_dir: Some(cache_dir.into()),
            bundle_setup_timeout: BUNDLE_SETUP_TIMEOUT,
        }
    }

    pub fn with_bundle_setup_timeout(mut self, timeout: Duration) -> Self {
        self.bundle_setup_timeout = timeout;
        self
    }

    fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(default_cache_dir)
    }

    fn bundle_dir(&self, version: &str) -> PathBuf {
        self.cache_dir().join("codeql").join(version)
    }

    fn bootstrap_bundle(&self, bundle_dir: &Path) -> Result<(), ManagedToolCacheError> {
        let setup_deadline = Instant::now() + self.bundle_setup_timeout;
        let bundle_root = bundle_dir.parent().ok_or_else(|| {
            bootstrap_extract_error(
                self.manifest.version.clone(),
                None,
                io::Error::other("bundle directory has no parent"),
            )
        })?;
        fs::create_dir_all(bundle_root).map_err(|source| {
            bootstrap_extract_error(self.manifest.version.clone(), Some(bundle_root), source)
        })?;
        if self.bundle_marker_matches_manifest(bundle_dir)? {
            return Ok(());
        }
        self.fail_fast_if_previous_no_space(bundle_root)?;
        let _lock = BundleBootstrapLock::acquire_before(
            bundle_root,
            &self.manifest.version,
            setup_deadline,
            self.bundle_setup_timeout,
        )?;
        if self.bundle_marker_matches_manifest(bundle_dir)? {
            return Ok(());
        }
        self.fail_fast_if_previous_no_space(bundle_root)?;

        let archive_path = archive_cache_path(bundle_root, &self.manifest.version);
        if self.archive_matches_manifest_before(&archive_path, setup_deadline)? {
            eprintln!(
                "Reusing cached CodeQL bundle archive {}",
                archive_path.display()
            );
        } else if let Err(error) = self.download_archive_before(&archive_path, setup_deadline) {
            self.record_bootstrap_failure_if_no_space(bundle_root, &error);
            return Err(error);
        }
        let install_result =
            self.install_bundle_from_archive_before(&archive_path, bundle_dir, setup_deadline);
        if let Err(error) = install_result {
            self.record_bootstrap_failure_if_no_space(bundle_root, &error);
            return Err(error);
        }
        remove_file_if_exists(&bootstrap_failure_path(bundle_root, &self.manifest.version))
            .map_err(|source| {
                bootstrap_extract_error(self.manifest.version.clone(), Some(bundle_root), source)
            })?;

        eprintln!(
            "CodeQL bundle v{} installed to {}",
            self.manifest.version,
            bundle_dir.display()
        );
        Ok(())
    }

    fn bundle_marker_matches_manifest(
        &self,
        bundle_dir: &Path,
    ) -> Result<bool, ManagedToolCacheError> {
        let marker_path = bundle_dir.join(BUNDLE_MARKER_FILE);
        if !marker_path.exists() {
            return Ok(false);
        }

        let marker_content = fs::read_to_string(&marker_path).map_err(|source| {
            ManagedToolCacheError::ReadBundleMarker {
                path: marker_path,
                source,
            }
        })?;
        Ok(marker_content.trim() == self.manifest.sha256)
    }

    fn archive_matches_manifest_before(
        &self,
        archive_path: &Path,
        deadline: Instant,
    ) -> Result<bool, ManagedToolCacheError> {
        if !archive_path.exists() {
            return Ok(false);
        }

        let cache_dir = archive_path.parent().unwrap_or_else(|| Path::new("."));
        let archive = File::open(archive_path).map_err(|source| {
            bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
        })?;
        let mut archive = DeadlineReader::new(
            archive,
            deadline,
            self.bundle_setup_timeout,
            "cached archive checksum",
        );
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let read = archive.read(&mut buffer).map_err(|source| {
                bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }

        let actual = format!("{:x}", hasher.finalize());
        if actual == self.manifest.sha256 {
            Ok(true)
        } else {
            remove_file_if_exists(archive_path).map_err(|source| {
                bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
            })?;
            Ok(false)
        }
    }

    #[cfg(test)]
    fn download_archive_with_timeout(
        &self,
        archive_path: &Path,
        timeout: Duration,
    ) -> Result<(), ManagedToolCacheError> {
        self.download_archive_before(archive_path, Instant::now() + timeout)
    }

    fn download_archive_before(
        &self,
        archive_path: &Path,
        deadline: Instant,
    ) -> Result<(), ManagedToolCacheError> {
        let timeout = match remaining_bundle_setup_timeout(
            deadline,
            self.bundle_setup_timeout,
            "download phase",
        ) {
            Ok(timeout) => timeout,
            Err(source) => {
                return Err(bootstrap_extract_error(
                    self.manifest.version.clone(),
                    archive_path.parent(),
                    source,
                ));
            }
        };
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_body(Some(Duration::from_secs(30).min(timeout)))
            .timeout_global(Some(timeout))
            .build()
            .new_agent();
        let response = agent
            .get(&self.manifest.download_url)
            .call()
            .map_err(|error| {
                self.bootstrap_download_error(archive_path, None, error.to_string())
            })?;
        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let mut reader = response.into_body().into_reader();
        let temp_archive_path = temp_archive_path(archive_path);
        let mut writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_archive_path)
            .map_err(|error| {
                self.bootstrap_download_error(archive_path, content_length, error.to_string())
            })?;

        let progress_message = if let Some(content_length) = content_length {
            format!(
                "Downloading CodeQL bundle v{} ({})",
                self.manifest.version,
                format_bytes(content_length)
            )
        } else {
            format!("Downloading CodeQL bundle v{}", self.manifest.version)
        };
        eprintln!(
            "CodeQL bundle setup is a cold/cache-heavy phase; using a bounded download timeout of {}. Reuse --cache-dir or pre-populate the managed CodeQL cache when running under a harness timeout.",
            format_duration(timeout)
        );
        let progress_bar = if let Some(content_length) = content_length {
            let progress_bar = ProgressBar::new(content_length);
            progress_bar.set_style(
                ProgressStyle::with_template(
                    "{msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                )
                .expect("valid progress template"),
            );
            progress_bar
        } else {
            let progress_bar = ProgressBar::new_spinner();
            progress_bar.enable_steady_tick(Duration::from_millis(100));
            progress_bar.set_style(
                ProgressStyle::with_template("{msg} {spinner} {bytes} ({bytes_per_sec})")
                    .expect("valid progress template"),
            );
            progress_bar
        };
        progress_bar.set_message(progress_message);

        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 16 * 1024];
        let download_result = (|| {
            loop {
                let read = reader.read(&mut buffer).map_err(|error| {
                    self.bootstrap_download_error(archive_path, content_length, error.to_string())
                })?;
                if read == 0 {
                    break;
                }
                progress_bar.inc(read as u64);
                hasher.update(&buffer[..read]);
                writer.write_all(&buffer[..read]).map_err(|error| {
                    self.bootstrap_download_error(archive_path, content_length, error.to_string())
                })?;
            }
            writer.flush().map_err(|error| {
                self.bootstrap_download_error(archive_path, content_length, error.to_string())
            })?;
            Ok::<_, ManagedToolCacheError>(format!("{:x}", hasher.finalize()))
        })();
        drop(writer);
        let actual = match download_result {
            Ok(actual) => actual,
            Err(error) => {
                let _ = remove_file_if_exists(&temp_archive_path);
                return Err(error);
            }
        };
        progress_bar.finish_with_message(format!(
            "Downloaded CodeQL bundle v{}",
            self.manifest.version
        ));

        if actual != self.manifest.sha256 {
            let _ = remove_file_if_exists(&temp_archive_path);
            return Err(ManagedToolCacheError::ChecksumMismatch {
                path: temp_archive_path,
                expected: self.manifest.sha256.clone(),
                actual,
            });
        }

        if let Err(error) =
            self.publish_downloaded_archive_before(&temp_archive_path, archive_path, deadline)
        {
            let _ = remove_file_if_exists(&temp_archive_path);
            return Err(error);
        }

        Ok(())
    }

    fn fail_fast_if_previous_no_space(
        &self,
        bundle_root: &Path,
    ) -> Result<(), ManagedToolCacheError> {
        let marker_path = bootstrap_failure_path(bundle_root, &self.manifest.version);
        let content = match fs::read_to_string(&marker_path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(bootstrap_extract_error(
                    self.manifest.version.clone(),
                    Some(bundle_root),
                    source,
                ));
            }
        };

        if !is_no_space_error(&content) {
            return Ok(());
        }

        Err(bootstrap_extract_error(
            self.manifest.version.clone(),
            Some(bundle_root),
            io::Error::other(format!(
                "previous CodeQL bundle bootstrap failed due to ENOSPC; fail fast before retrying download/extraction. Remove `{}` after freeing space, or clean the managed cache and rerun",
                marker_path.display()
            )),
        ))
    }

    fn record_bootstrap_failure_if_no_space(
        &self,
        bundle_root: &Path,
        error: &ManagedToolCacheError,
    ) {
        let message = error.to_string();
        if !is_no_space_error(&message) {
            return;
        }

        let marker_path = bootstrap_failure_path(bundle_root, &self.manifest.version);
        let content = format!(
            "kind=enospc\nversion={}\nmessage={}\n",
            self.manifest.version, message
        );
        let _ = fs::write(marker_path, content);
    }

    fn publish_downloaded_archive_before(
        &self,
        temp_archive_path: &Path,
        archive_path: &Path,
        deadline: Instant,
    ) -> Result<(), ManagedToolCacheError> {
        match fs::hard_link(temp_archive_path, archive_path) {
            Ok(()) => {
                remove_file_if_exists(temp_archive_path).map_err(|source| {
                    bootstrap_extract_error(
                        self.manifest.version.clone(),
                        archive_path.parent(),
                        source,
                    )
                })?;
                Ok(())
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                remove_file_if_exists(temp_archive_path).map_err(|source| {
                    bootstrap_extract_error(
                        self.manifest.version.clone(),
                        archive_path.parent(),
                        source,
                    )
                })?;
                if self.archive_matches_manifest_before(archive_path, deadline)? {
                    eprintln!(
                        "Reusing concurrently cached CodeQL bundle archive {}",
                        archive_path.display()
                    );
                    Ok(())
                } else {
                    Err(ManagedToolCacheError::ChecksumMismatch {
                        path: archive_path.to_path_buf(),
                        expected: self.manifest.sha256.clone(),
                        actual: "concurrent archive did not match manifest".to_owned(),
                    })
                }
            }
            Err(source) => Err(bootstrap_extract_error(
                self.manifest.version.clone(),
                archive_path.parent(),
                source,
            )),
        }
    }

    fn bootstrap_download_error(
        &self,
        archive_path: &Path,
        content_length: Option<u64>,
        message: String,
    ) -> ManagedToolCacheError {
        let cache_dir = archive_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let guidance =
            bootstrap_download_guidance(archive_path, &cache_dir, content_length, &message);
        ManagedToolCacheError::BootstrapDownload {
            version: self.manifest.version.clone(),
            url: self.manifest.download_url.clone(),
            archive_path: archive_path.to_path_buf(),
            cache_dir,
            content_length,
            message,
            guidance,
        }
    }

    fn install_bundle_from_archive_before(
        &self,
        archive_path: &Path,
        bundle_dir: &Path,
        deadline: Instant,
    ) -> Result<(), ManagedToolCacheError> {
        let cache_dir = bundle_dir.parent().unwrap_or(bundle_dir);
        let archive = File::open(archive_path).map_err(|source| {
            bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
        })?;
        self.install_bundle_from_reader_before(archive, bundle_dir, deadline)
    }

    #[cfg(test)]
    fn install_bundle_from_reader<R: Read>(
        &self,
        archive: R,
        bundle_dir: &Path,
    ) -> Result<(), ManagedToolCacheError> {
        self.install_bundle_from_reader_before(
            archive,
            bundle_dir,
            Instant::now() + BUNDLE_SETUP_TIMEOUT,
        )
    }

    fn install_bundle_from_reader_before<R: Read>(
        &self,
        archive: R,
        bundle_dir: &Path,
        deadline: Instant,
    ) -> Result<(), ManagedToolCacheError> {
        let staging_dir = staging_dir(bundle_dir, &self.manifest.version);
        let cache_dir = bundle_dir.parent().unwrap_or(bundle_dir);
        if staging_dir.exists() {
            fs::remove_dir_all(&staging_dir).map_err(|source| {
                bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
            })?;
        }
        fs::create_dir_all(&staging_dir).map_err(|source| {
            bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
        })?;

        let install_result = self
            .unpack_archive_into_before(archive, &staging_dir, deadline)
            .and_then(|_| {
                fs::write(
                    staging_dir.join(BUNDLE_MARKER_FILE),
                    self.manifest.sha256.as_bytes(),
                )
                .map_err(|source| {
                    bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
                })
            });

        if let Err(error) = install_result {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }

        if bundle_dir.exists() {
            fs::remove_dir_all(bundle_dir).map_err(|source| {
                bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
            })?;
        }
        fs::rename(&staging_dir, bundle_dir).map_err(|source| {
            bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
        })?;

        Ok(())
    }

    fn unpack_archive_into_before<R: Read>(
        &self,
        archive: R,
        destination: &Path,
        deadline: Instant,
    ) -> Result<(), ManagedToolCacheError> {
        let cache_dir = destination.parent().unwrap_or(destination);
        let decoder = GzDecoder::new(DeadlineReader::new(
            archive,
            deadline,
            self.bundle_setup_timeout,
            "extract/install phase",
        ));
        let mut tar = Archive::new(decoder);
        let mut extracted_entry = false;

        for entry in tar.entries().map_err(|source| {
            bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
        })? {
            let mut entry = entry.map_err(|source| {
                bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
            })?;
            let entry_path = entry.path().map_err(|source| {
                bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
            })?;
            let stripped = entry_path.strip_prefix("codeql").map_err(|_| {
                bootstrap_extract_error(
                    self.manifest.version.clone(),
                    Some(cache_dir),
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "archive entry `{}` does not start with `codeql/`",
                            entry_path.display()
                        ),
                    ),
                )
            })?;
            let relative_path = sanitize_archive_path(stripped).map_err(|source| {
                bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
            })?;
            if relative_path.as_os_str().is_empty() {
                continue;
            }

            let output_path = destination.join(relative_path);
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent).map_err(|source| {
                    bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
                })?;
            }

            if entry.header().entry_type().is_dir() {
                fs::create_dir_all(&output_path).map_err(|source| {
                    bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
                })?;
            } else {
                entry.unpack(&output_path).map_err(|source| {
                    bootstrap_extract_error(self.manifest.version.clone(), Some(cache_dir), source)
                })?;
            }
            extracted_entry = true;
        }

        if !extracted_entry {
            return Err(bootstrap_extract_error(
                self.manifest.version.clone(),
                Some(cache_dir),
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive did not contain any extractable entries under `codeql/`",
                ),
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ManagedToolCacheError {
    #[error("unsupported tool cache lookup for `{tool_name}`; only `codeql` is supported")]
    UnsupportedTool { tool_name: String },
    #[error(
        "requested CodeQL bundle version `{requested}` does not match manifest version `{available}`"
    )]
    UnsupportedVersion {
        requested: String,
        available: String,
    },
    #[error("failed to read cached bundle marker `{path}`: {source}")]
    ReadBundleMarker {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "cached bundle marker `{path}` checksum mismatch: expected `{expected}`, got `{actual}`"
    )]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("failed to download CodeQL bundle v{version} from {url}: {message}. {guidance}")]
    BootstrapDownload {
        version: String,
        url: String,
        archive_path: PathBuf,
        cache_dir: PathBuf,
        content_length: Option<u64>,
        message: String,
        guidance: String,
    },
    #[error("failed to extract CodeQL bundle v{version}: {source}. {guidance}")]
    BootstrapExtract {
        version: String,
        cache_dir: Option<PathBuf>,
        #[source]
        source: io::Error,
        guidance: String,
    },
    #[error("unsupported platform for managed CodeQL bundle")]
    UnsupportedPlatform,
}

impl ToolCachePort for ManagedToolCacheAdapter {
    type Error = ManagedToolCacheError;

    fn resolve_bundle(
        &self,
        request: &ToolCacheRequest,
    ) -> Result<ResolvedToolBundle, Self::Error> {
        if request.tool_name != "codeql" {
            return Err(ManagedToolCacheError::UnsupportedTool {
                tool_name: request.tool_name.clone(),
            });
        }

        if request.version != self.manifest.version {
            return Err(ManagedToolCacheError::UnsupportedVersion {
                requested: request.version.clone(),
                available: self.manifest.version.clone(),
            });
        }

        let cache_path = self.bundle_dir(&request.version);
        let marker_path = cache_path.join(BUNDLE_MARKER_FILE);
        if !cache_path.exists() || !marker_path.exists() {
            self.bootstrap_bundle(&cache_path)?;
        }

        let marker_content = fs::read_to_string(&marker_path).map_err(|source| {
            ManagedToolCacheError::ReadBundleMarker {
                path: marker_path.clone(),
                source,
            }
        })?;
        let actual = marker_content.trim().to_owned();
        if actual != self.manifest.sha256 {
            return Err(ManagedToolCacheError::ChecksumMismatch {
                path: marker_path,
                expected: self.manifest.sha256.clone(),
                actual,
            });
        }

        let cache_root = cache_path.parent().unwrap_or(&cache_path);
        deploy_bundled_queries(&cache_path).map_err(|source| {
            bootstrap_extract_error(self.manifest.version.clone(), Some(cache_root), source)
        })?;

        Ok(ResolvedToolBundle {
            tool_name: request.tool_name.clone(),
            version: request.version.clone(),
            cache_path,
            checksum: self.manifest.sha256.clone(),
        })
    }
}

fn deploy_bundled_queries(bundle_dir: &Path) -> io::Result<()> {
    let queries_dir = bundle_dir.join("queries");
    fs::create_dir_all(&queries_dir)?;
    for (filename, content) in BUNDLED_QUERIES {
        let lang_dir = filename
            .strip_prefix("extract-")
            .and_then(|name| name.strip_suffix(".ql"))
            .unwrap_or(filename);
        let subdir = queries_dir.join(lang_dir);
        fs::create_dir_all(&subdir)?;
        fs::write(subdir.join(filename), content)?;
    }
    for (lang_dir, qlpack_content) in BUNDLED_QLPACKS {
        let subdir = queries_dir.join(lang_dir);
        fs::create_dir_all(&subdir)?;
        fs::write(subdir.join("qlpack.yml"), qlpack_content)?;
    }
    Ok(())
}

fn default_cache_dir() -> PathBuf {
    if let Some(cache_dir) = env::var_os("KALOS_CACHE_DIR") {
        return PathBuf::from(cache_dir);
    }

    if let Some(home_dir) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        return Path::new(&home_dir).join(".cache").join("kalos");
    }

    PathBuf::from(".cache").join("kalos")
}

fn sanitize_archive_path(path: &Path) -> io::Result<PathBuf> {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => cleaned.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported archive path component in `{}`", path.display()),
                ));
            }
        }
    }

    Ok(cleaned)
}

fn archive_cache_path(bundle_root: &Path, version: &str) -> PathBuf {
    bundle_root.join(format!(".codeql-bundle-{version}.tar.gz"))
}

fn bootstrap_failure_path(bundle_root: &Path, version: &str) -> PathBuf {
    bundle_root.join(format!(
        ".codeql-bundle-{version}.{BUNDLE_BOOTSTRAP_FAILURE_FILE}"
    ))
}

fn temp_archive_path(archive_path: &Path) -> PathBuf {
    let file_name = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("codeql-bundle.tar.gz");
    archive_path.with_file_name(format!("{file_name}-{}", unique_suffix()))
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

struct DeadlineReader<R> {
    inner: R,
    deadline: Instant,
    setup_timeout: Duration,
    phase: &'static str,
}

impl<R> DeadlineReader<R> {
    fn new(inner: R, deadline: Instant, setup_timeout: Duration, phase: &'static str) -> Self {
        Self {
            inner,
            deadline,
            setup_timeout,
            phase,
        }
    }
}

impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        remaining_bundle_setup_timeout(self.deadline, self.setup_timeout, self.phase)?;
        self.inner.read(buffer)
    }
}

fn remaining_bundle_setup_timeout(
    deadline: Instant,
    setup_timeout: Duration,
    phase: &'static str,
) -> io::Result<Duration> {
    let now = Instant::now();
    if now >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "CodeQL bundle setup timed out during {phase}; setup timeout is {}",
                format_duration(setup_timeout)
            ),
        ));
    }
    Ok(deadline.duration_since(now))
}

fn bootstrap_download_guidance(
    archive_path: &Path,
    cache_dir: &Path,
    content_length: Option<u64>,
    message: &str,
) -> String {
    let capacity = content_length
        .map(|length| {
            format!(
                " The download requires at least {} for the archive plus extraction space.",
                format_bytes(length)
            )
        })
        .unwrap_or_else(|| {
            " The download requires enough free space for the archive plus extraction space."
                .to_owned()
        });
    let cleanup = format!(
        "Ensure network connectivity or pre-populate the CodeQL bundle cache.{} Retry after freeing space or cleaning the managed cache with `rm -f {}` or `rm -rf {}`.",
        capacity,
        shell_quote_path(archive_path),
        shell_quote_path(cache_dir)
    );
    if is_no_space_error(message) {
        format!("No space left while writing the CodeQL archive. {cleanup}")
    } else if is_timeout_error(message) {
        format!("Timed out during the cold/cache-heavy CodeQL bundle download. {cleanup}")
    } else {
        cleanup
    }
}

fn bootstrap_extract_error(
    version: String,
    cache_dir: Option<&Path>,
    source: io::Error,
) -> ManagedToolCacheError {
    let message = source.to_string();
    let guidance = bootstrap_extract_guidance(cache_dir, &message);
    ManagedToolCacheError::BootstrapExtract {
        version,
        cache_dir: cache_dir.map(Path::to_path_buf),
        source,
        guidance,
    }
}

fn stale_bootstrap_lock_error(
    version: String,
    cache_dir: &Path,
    lock_path: &Path,
    timeout: Duration,
) -> ManagedToolCacheError {
    bootstrap_extract_error(
        version,
        Some(cache_dir),
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "CodeQL bundle bootstrap lock `{}` showed no progress for {}ms; remove the stale lock directory and retry",
                lock_path.display(),
                timeout.as_millis()
            ),
        ),
    )
}

fn bootstrap_lock_wait_timeout_error(
    version: String,
    cache_dir: &Path,
    lock_path: &Path,
    source: io::Error,
) -> ManagedToolCacheError {
    bootstrap_extract_error(
        version,
        Some(cache_dir),
        io::Error::new(
            source.kind(),
            format!(
                "{}; still waiting for bootstrap lock `{}`. If no kalos process is actively bootstrapping this cache, remove the lock directory and retry",
                source,
                lock_path.display()
            ),
        ),
    )
}

fn bootstrap_extract_guidance(cache_dir: Option<&Path>, message: &str) -> String {
    let cleanup = if let Some(cache_dir) = cache_dir {
        format!(
            "Retry after freeing space or cleaning the managed CodeQL cache with `rm -rf {}`.",
            shell_quote_path(cache_dir)
        )
    } else {
        "Retry after freeing space or cleaning the managed CodeQL cache directory.".to_owned()
    };

    if is_no_space_error(message) {
        format!("No space left while extracting the CodeQL bundle. {cleanup}")
    } else if is_timeout_error(message) {
        format!(
            "Timed out during cold/cache-heavy CodeQL bundle setup or extraction. Reuse --cache-dir or pre-populate the managed CodeQL cache before running under a harness timeout. {cleanup}"
        )
    } else {
        format!("If this was caused by ENOSPC/no space left on device, {cleanup}")
    }
}

fn is_no_space_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("no space left")
        || message.contains("os error 28")
        || message.contains("enospc")
}

fn is_timeout_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("timeout")
        || message.contains("timed out")
        || message.contains("deadline")
        || message.contains("elapsed")
}

fn shell_quote_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() < 60 {
        let secs = duration.as_secs();
        let tenths = duration.subsec_millis() / 100;
        format!("{secs}.{tenths}s")
    } else {
        let secs = duration.as_secs();
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn staging_dir(bundle_dir: &Path, version: &str) -> PathBuf {
    let bundle_root = bundle_dir.parent().unwrap_or(bundle_dir);
    bundle_root.join(format!(".codeql-bundle-{version}-{}", unique_suffix()))
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{self, Cursor, Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    use std::thread;
    use std::time::{Duration, Instant};

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use sha2::{Digest, Sha256};
    use tar::{Builder, Header};
    use tempfile::TempDir;

    use super::{
        BUNDLE_MARKER_FILE, BUNDLE_SETUP_TIMEOUT, BUNDLED_QLPACKS, BUNDLED_QUERIES,
        BootstrapLockState, BundleBootstrapLock, BundleManifest, ManagedToolCacheAdapter,
        ManagedToolCacheError, Platform, archive_cache_path, bootstrap_download_guidance,
        bootstrap_extract_error, bootstrap_failure_path, bootstrap_lock_state,
        codeql_bundle_manifest, deploy_bundled_queries,
    };
    use crate::ports::tool_cache::{ToolCachePort, ToolCacheRequest};

    fn bundled_query_lang_dir(filename: &str) -> &str {
        filename
            .strip_prefix("extract-")
            .and_then(|name| name.strip_suffix(".ql"))
            .unwrap_or(filename)
    }

    #[test]
    fn codeql_bundle_manifest_returns_pinned_supported_manifest() {
        let manifest = codeql_bundle_manifest().unwrap();

        assert_eq!(manifest.version, "2.25.1");
        assert_eq!(manifest.sha256.len(), 64);
        match Platform::detect().unwrap() {
            Platform::LinuxX64 | Platform::LinuxArm64 => {
                assert!(
                    manifest
                        .download_url
                        .ends_with("codeql-bundle-linux64.tar.gz")
                );
            }
            Platform::MacosX64 | Platform::MacosArm64 => {
                assert!(
                    manifest
                        .download_url
                        .ends_with("codeql-bundle-osx64.tar.gz")
                );
            }
            Platform::WindowsX64 => {
                assert!(
                    manifest
                        .download_url
                        .ends_with("codeql-bundle-win64.tar.gz")
                );
            }
        }
    }

    #[test]
    fn platform_is_emulated_returns_true_for_arm64_variants() {
        assert!(Platform::MacosArm64.is_emulated());
        assert!(Platform::LinuxArm64.is_emulated());
        assert!(!Platform::MacosX64.is_emulated());
        assert!(!Platform::LinuxX64.is_emulated());
        assert!(!Platform::WindowsX64.is_emulated());
    }

    #[test]
    fn platform_emulation_notice_present_for_arm64_variants() {
        assert!(Platform::MacosArm64.emulation_notice().is_some());
        assert!(Platform::LinuxArm64.emulation_notice().is_some());
        assert!(Platform::MacosX64.emulation_notice().is_none());
        assert!(Platform::LinuxX64.emulation_notice().is_none());
        assert!(Platform::WindowsX64.emulation_notice().is_none());
    }

    #[test]
    fn platform_emulation_notice_mentions_rosetta_for_macos_arm64() {
        let notice = Platform::MacosArm64.emulation_notice().unwrap();
        assert!(notice.contains("Rosetta 2"));
    }

    #[test]
    fn resolve_bundle_returns_cache_hit() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        fs::create_dir_all(&bundle_dir).unwrap();
        let checksum = "a".repeat(64);
        fs::write(bundle_dir.join(BUNDLE_MARKER_FILE), &checksum).unwrap();
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        let bundle = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap();

        assert_eq!(bundle.cache_path, bundle_dir);
        assert_eq!(bundle.checksum, checksum);
    }

    #[test]
    fn bootstrap_extracts_and_writes_marker() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        adapter
            .install_bundle_from_reader(Cursor::new(archive_bytes), &bundle_dir)
            .unwrap();

        assert_eq!(
            fs::read_to_string(bundle_dir.join(BUNDLE_MARKER_FILE))
                .unwrap()
                .trim(),
            checksum
        );
        assert_eq!(
            fs::read_to_string(bundle_dir.join("queries").join("extract-rust.ql")).unwrap(),
            "// fixture query\n"
        );
        assert!(bundle_dir.join("codeql").exists());
    }

    #[test]
    fn resolve_bundle_bootstraps_bundle_on_cache_miss() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let (download_url, server) = spawn_http_server(archive_bytes);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url,
            },
            temp.path(),
        );

        let bundle = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap();
        server.join().unwrap();

        assert_eq!(bundle.checksum, checksum);
        assert!(bundle.cache_path.join("codeql").exists());
        assert!(
            bundle
                .cache_path
                .join("queries")
                .join("rust")
                .join("extract-rust.ql")
                .exists()
        );
        assert!(
            bundle
                .cache_path
                .join("queries")
                .join("rust")
                .join("qlpack.yml")
                .exists()
        );
        assert_eq!(
            fs::read_to_string(bundle.cache_path.join(BUNDLE_MARKER_FILE))
                .unwrap()
                .trim(),
            checksum
        );
    }

    #[test]
    fn bootstrap_uses_deterministic_archive_cache_path() {
        let temp = TempDir::new().unwrap();
        let bundle_root = temp.path().join("codeql");

        assert_eq!(
            archive_cache_path(&bundle_root, "2.0.0"),
            bundle_root.join(".codeql-bundle-2.0.0.tar.gz")
        );
    }

    #[test]
    fn resolve_bundle_reuses_cached_archive_without_redownloading() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        fs::write(archive_cache_path(&bundle_root, "2.0.0"), archive_bytes).unwrap();
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url: "http://127.0.0.1:1/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        let bundle = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap();

        assert_eq!(bundle.checksum, checksum);
        assert!(bundle.cache_path.join("codeql").exists());
        assert!(archive_cache_path(&bundle_root, "2.0.0").exists());
    }

    #[test]
    fn resolve_bundle_recovers_when_stale_lock_file_remains() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let stale_lock_path = bundle_root.join(".codeql-bundle-2.0.0.lock");
        fs::write(&stale_lock_path, "pid=999999\n").unwrap();
        fs::write(archive_cache_path(&bundle_root, "2.0.0"), archive_bytes).unwrap();
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url: "http://127.0.0.1:1/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        let bundle = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap();

        assert_eq!(bundle.checksum, checksum);
        assert!(bundle.cache_path.join("codeql").exists());
        assert!(stale_lock_path.exists());
        assert_eq!(
            fs::read_to_string(bundle.cache_path.join(BUNDLE_MARKER_FILE))
                .unwrap()
                .trim(),
            checksum
        );
    }

    #[test]
    fn bundle_bootstrap_lock_times_out_with_stale_lock_directory_guidance() {
        let temp = TempDir::new().unwrap();
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let lock_path = bundle_root.join(".codeql-bundle-2.0.0.lock.d");
        fs::create_dir(&lock_path).unwrap();

        let error = BundleBootstrapLock::acquire_with_timeout(
            &bundle_root,
            "2.0.0",
            Duration::from_millis(0),
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("failed to extract CodeQL bundle v2.0.0"));
        assert!(message.contains("showed no progress for 0ms"));
        assert!(message.contains("remove the stale lock directory and retry"));
        assert!(message.contains(&lock_path.display().to_string()));
        assert!(message.contains(&format!("rm -rf '{}'", bundle_root.display())));
        assert!(lock_path.exists());
    }

    #[test]
    fn bundle_bootstrap_lock_treats_deleted_lock_directory_as_missing() {
        let temp = TempDir::new().unwrap();
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let lock_path = bundle_root.join(".codeql-bundle-2.0.0.lock.d");
        fs::create_dir(&lock_path).unwrap();
        fs::remove_dir_all(&lock_path).unwrap();

        assert_eq!(
            bootstrap_lock_state(&lock_path, Duration::ZERO),
            BootstrapLockState::Missing
        );

        let lock = BundleBootstrapLock::acquire_with_timeout(&bundle_root, "2.0.0", Duration::ZERO)
            .unwrap();
        assert!(lock_path.exists());
        drop(lock);
    }

    #[test]
    fn bundle_bootstrap_lock_waits_while_existing_lock_heartbeats() {
        let temp = TempDir::new().unwrap();
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let holder =
            BundleBootstrapLock::acquire_with_timeout(&bundle_root, "2.0.0", Duration::ZERO)
                .unwrap();
        let waiter_bundle_root = bundle_root.clone();

        let waiter = thread::spawn(move || {
            BundleBootstrapLock::acquire_with_timeout(
                &waiter_bundle_root,
                "2.0.0",
                Duration::from_millis(100),
            )
        });
        thread::sleep(Duration::from_millis(250));
        drop(holder);

        let waited_lock = waiter.join().unwrap().unwrap();
        drop(waited_lock);
    }

    #[test]
    fn bundle_bootstrap_lock_wait_is_bounded_by_setup_deadline() {
        let temp = TempDir::new().unwrap();
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let holder =
            BundleBootstrapLock::acquire_with_timeout(&bundle_root, "2.0.0", Duration::ZERO)
                .unwrap();
        let lock_path = bundle_root.join(".codeql-bundle-2.0.0.lock.d");
        let expired_soon = Instant::now() + Duration::from_millis(75);

        let error = BundleBootstrapLock::acquire_with_timeout_before(
            &bundle_root,
            "2.0.0",
            Duration::from_secs(30),
            expired_soon,
            Duration::from_millis(75),
        )
        .unwrap_err();
        drop(holder);

        let message = error.to_string();
        assert!(message.contains("bootstrap lock wait"));
        assert!(message.contains("cold/cache-heavy CodeQL bundle setup or extraction"));
        assert!(message.contains("remove the lock directory and retry"));
        assert!(message.contains(&lock_path.display().to_string()));
        assert!(message.contains("rm -rf"));
    }

    #[test]
    fn resolve_bundle_removes_corrupt_cached_archive_before_redownload() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let archive_path = archive_cache_path(&bundle_root, "2.0.0");
        fs::write(&archive_path, b"corrupt archive").unwrap();
        let (download_url, server) = spawn_http_server(archive_bytes);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum.clone(),
                download_url,
            },
            temp.path(),
        );

        let bundle = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap();
        server.join().unwrap();

        assert_eq!(bundle.checksum, checksum);
        assert_eq!(
            format!("{:x}", Sha256::digest(fs::read(&archive_path).unwrap())),
            checksum
        );
    }

    #[test]
    fn concurrent_resolve_bundle_cold_cache_downloads_once_and_reuses_bundle() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let request_count = Arc::new(AtomicUsize::new(0));
        let (download_url, server) =
            spawn_counting_http_server(archive_bytes, Arc::clone(&request_count));
        let barrier = Arc::new(Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let adapter = ManagedToolCacheAdapter::with_cache_dir(
                    BundleManifest {
                        version: "2.0.0".to_owned(),
                        sha256: checksum.clone(),
                        download_url: download_url.clone(),
                    },
                    temp.path(),
                );
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    adapter.resolve_bundle(&ToolCacheRequest {
                        tool_name: "codeql".to_owned(),
                        version: "2.0.0".to_owned(),
                    })
                })
            })
            .collect();

        let bundles: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect();
        server.join().unwrap();

        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        for bundle in bundles {
            assert_eq!(bundle.checksum, checksum);
            assert!(bundle.cache_path.join("codeql").exists());
            assert_eq!(
                fs::read_to_string(bundle.cache_path.join(BUNDLE_MARKER_FILE))
                    .unwrap()
                    .trim(),
                checksum
            );
        }
        let bundle_root = temp.path().join("codeql");
        assert!(archive_cache_path(&bundle_root, "2.0.0").exists());
        let temp_archives: Vec<_> = fs::read_dir(&bundle_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".codeql-bundle-2.0.0.tar.gz-"))
            .collect();
        assert!(
            temp_archives.is_empty(),
            "unexpected temp archives: {temp_archives:?}"
        );
    }

    #[test]
    fn publish_download_collision_checksum_uses_remaining_setup_deadline() {
        let temp = TempDir::new().unwrap();
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let archive_path = archive_cache_path(&bundle_root, "2.0.0");
        let temp_archive_path = temp.path().join("downloaded.tar.gz");
        fs::write(&archive_path, fixture_archive_bytes()).unwrap();
        fs::write(&temp_archive_path, fixture_archive_bytes()).unwrap();
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "a".repeat(64),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );
        let expired_deadline = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);

        let error = adapter
            .publish_downloaded_archive_before(&temp_archive_path, &archive_path, expired_deadline)
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("cached archive checksum"));
        assert!(message.contains("cold/cache-heavy CodeQL bundle setup or extraction"));
        assert!(!temp_archive_path.exists());
    }

    #[test]
    fn download_archive_reports_progress() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let (download_url, server) = spawn_http_server(archive_bytes);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum,
                download_url,
            },
            temp.path(),
        );
        let archive_path = temp.path().join("codeql.tar.gz");

        adapter
            .download_archive_with_timeout(&archive_path, BUNDLE_SETUP_TIMEOUT)
            .unwrap();
        server.join().unwrap();

        assert!(archive_path.exists());
    }

    #[test]
    fn download_archive_works_without_content_length() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let (download_url, server) = spawn_http_server_no_content_length(archive_bytes);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum,
                download_url,
            },
            temp.path(),
        );
        let archive_path = temp.path().join("codeql.tar.gz");

        adapter
            .download_archive_with_timeout(&archive_path, BUNDLE_SETUP_TIMEOUT)
            .unwrap();
        server.join().unwrap();

        assert!(archive_path.exists());
    }

    #[test]
    fn download_archive_timeout_reports_cache_heavy_guidance() {
        let temp = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            thread::sleep(Duration::from_millis(200));
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nslow"
            );
            let _ = stream.flush();
        });
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "a".repeat(64),
                download_url: format!("http://{addr}/codeql.tgz"),
            },
            temp.path(),
        );
        let archive_path = temp.path().join("codeql.tar.gz");

        let error = adapter
            .download_archive_with_timeout(&archive_path, Duration::from_millis(50))
            .unwrap_err();
        server.join().unwrap();

        let message = error.to_string();
        assert!(message.contains("failed to download CodeQL bundle v2.0.0"));
        assert!(message.contains("cold/cache-heavy CodeQL bundle download"));
        assert!(message.contains("pre-populate the CodeQL bundle cache"));
        assert!(!archive_path.exists());
    }

    #[test]
    fn resolve_bundle_reports_download_error_without_bootstrap_hint() {
        let temp = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "a".repeat(64),
                download_url: format!("http://{addr}/codeql.tgz"),
            },
            temp.path(),
        );

        let error = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("failed to download CodeQL bundle v2.0.0"));
        assert!(
            message.contains("Ensure network connectivity or pre-populate the CodeQL bundle cache")
        );
        assert!(!message.contains("kalos bootstrap"));
    }

    #[test]
    fn resolve_bundle_cleans_up_deterministic_archive_on_download_error() {
        let temp = TempDir::new().unwrap();
        let (download_url, server) = spawn_truncated_http_server(b"partial".to_vec(), 32);
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "a".repeat(64),
                download_url,
            },
            temp.path(),
        );

        let error = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap_err();
        server.join().unwrap();

        assert!(
            error
                .to_string()
                .contains("failed to download CodeQL bundle v2.0.0")
        );
        assert!(!archive_cache_path(&temp.path().join("codeql"), "2.0.0").exists());
        let hidden_archives: Vec<_> = fs::read_dir(temp.path().join("codeql"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".codeql-bundle-2.0.0.tar.gz"))
            .collect();
        assert!(
            hidden_archives.is_empty(),
            "unexpected temp archives: {hidden_archives:?}"
        );
    }

    #[test]
    fn resolve_bundle_fails_fast_after_previous_enospc_bootstrap_failure() {
        let temp = TempDir::new().unwrap();
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "a".repeat(64),
                download_url: "http://127.0.0.1:1/codeql.tgz".to_owned(),
            },
            temp.path(),
        );
        fs::write(
            bootstrap_failure_path(&bundle_root, "2.0.0"),
            "kind=enospc\nmessage=No space left on device (os error 28)\n",
        )
        .unwrap();

        let error = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("failed to extract CodeQL bundle v2.0.0"));
        assert!(message.contains("previous CodeQL bundle bootstrap failed due to ENOSPC"));
        assert!(message.contains("fail fast before retrying download/extraction"));
        assert!(message.contains("No space left"));
        assert!(!archive_cache_path(&bundle_root, "2.0.0").exists());
        assert!(!bundle_root.join("2.0.0").exists());
    }

    #[test]
    fn no_space_bootstrap_error_records_failure_marker_for_later_fail_fast() {
        let temp = TempDir::new().unwrap();
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let archive_path = archive_cache_path(&bundle_root, "2.0.0");
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "a".repeat(64),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );
        let error = ManagedToolCacheError::BootstrapDownload {
            version: "2.0.0".to_owned(),
            url: "https://example.invalid/codeql.tgz".to_owned(),
            archive_path: archive_path.clone(),
            cache_dir: bundle_root.clone(),
            content_length: Some(1024),
            message: "No space left on device (os error 28)".to_owned(),
            guidance: bootstrap_download_guidance(
                &archive_path,
                &bundle_root,
                Some(1024),
                "No space left on device (os error 28)",
            ),
        };

        adapter.record_bootstrap_failure_if_no_space(&bundle_root, &error);

        let marker = bootstrap_failure_path(&bundle_root, "2.0.0");
        let marker_content = fs::read_to_string(marker).unwrap();
        assert!(marker_content.contains("kind=enospc"));
        assert!(marker_content.contains("No space left on device"));
    }

    #[test]
    fn bootstrap_download_error_includes_enospc_cleanup_guidance() {
        let archive_path = Path::new("/cache/kalos cache/codeql/.codeql-bundle-2.0.0.tar.gz");
        let cache_dir = Path::new("/cache/kalos cache/codeql");
        let guidance = bootstrap_download_guidance(
            archive_path,
            cache_dir,
            Some(3 * 1024 * 1024 * 1024),
            "No space left on device (os error 28)",
        );
        let error = ManagedToolCacheError::BootstrapDownload {
            version: "2.0.0".to_owned(),
            url: "https://example.invalid/codeql.tgz".to_owned(),
            archive_path: archive_path.to_path_buf(),
            cache_dir: cache_dir.to_path_buf(),
            content_length: Some(3 * 1024 * 1024 * 1024),
            message: "No space left on device (os error 28)".to_owned(),
            guidance,
        };

        let message = error.to_string();
        assert!(message.contains("No space left"));
        assert!(message.contains("3.0 GiB"));
        assert!(message.contains("rm -f '/cache/kalos cache/codeql/.codeql-bundle-2.0.0.tar.gz'"));
        assert!(message.contains("rm -rf '/cache/kalos cache/codeql'"));
        assert!(message.contains("pre-populate the CodeQL bundle cache"));
    }

    #[test]
    fn bootstrap_download_error_includes_timeout_cache_heavy_guidance() {
        let archive_path = Path::new("/cache/kalos/codeql/.codeql-bundle-2.0.0.tar.gz");
        let cache_dir = Path::new("/cache/kalos/codeql");
        let guidance =
            bootstrap_download_guidance(archive_path, cache_dir, Some(1024), "operation timed out");

        assert!(guidance.contains("Timed out during the cold/cache-heavy CodeQL bundle download"));
        assert!(guidance.contains("pre-populate the CodeQL bundle cache"));
        assert!(guidance.contains("rm -rf '/cache/kalos/codeql'"));
    }

    #[test]
    fn bootstrap_extract_error_includes_enospc_cleanup_guidance() {
        let cache_dir = Path::new("/cache/kalos cache/codeql");
        let error = bootstrap_extract_error(
            "2.0.0".to_owned(),
            Some(cache_dir),
            io::Error::from_raw_os_error(28),
        );

        let message = error.to_string();
        assert!(message.contains("failed to extract CodeQL bundle v2.0.0"));
        assert!(message.contains("No space left"));
        assert!(message.contains("rm -rf '/cache/kalos cache/codeql'"));
        assert!(message.contains("Retry after freeing space"));
    }

    #[test]
    fn cached_archive_extract_timeout_reports_cache_heavy_guidance() {
        let temp = TempDir::new().unwrap();
        let archive_bytes = fixture_archive_bytes();
        let checksum = format!("{:x}", Sha256::digest(&archive_bytes));
        let bundle_root = temp.path().join("codeql");
        fs::create_dir_all(&bundle_root).unwrap();
        let archive_path = archive_cache_path(&bundle_root, "2.0.0");
        fs::write(&archive_path, archive_bytes).unwrap();
        let bundle_dir = bundle_root.join("2.0.0");
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: checksum,
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );
        let expired_deadline = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);

        let error = adapter
            .install_bundle_from_archive_before(&archive_path, &bundle_dir, expired_deadline)
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("failed to extract CodeQL bundle v2.0.0"));
        assert!(message.contains("timed out during extract/install phase"));
        assert!(message.contains("cold/cache-heavy CodeQL bundle setup or extraction"));
        assert!(message.contains("pre-populate the managed CodeQL cache"));
        assert!(message.contains("rm -rf"));
        assert!(!bundle_dir.exists());
    }

    #[test]
    fn resolve_bundle_detects_checksum_mismatch() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(bundle_dir.join(BUNDLE_MARKER_FILE), "b".repeat(64)).unwrap();
        let adapter = ManagedToolCacheAdapter::with_cache_dir(
            BundleManifest {
                version: "2.0.0".to_owned(),
                sha256: "f".repeat(64),
                download_url: "https://example.invalid/codeql.tgz".to_owned(),
            },
            temp.path(),
        );

        let error = adapter
            .resolve_bundle(&ToolCacheRequest {
                tool_name: "codeql".to_owned(),
                version: "2.0.0".to_owned(),
            })
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("checksum mismatch"));
        assert!(message.contains(&bundle_dir.join(BUNDLE_MARKER_FILE).display().to_string()));
    }

    fn fixture_archive_bytes() -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = Builder::new(&mut encoder);
            append_tar_entry(&mut builder, "codeql/codeql", b"#!/bin/sh\nexit 0\n");
            append_tar_entry(
                &mut builder,
                "codeql/queries/extract-rust.ql",
                b"// fixture query\n",
            );
            builder.finish().unwrap();
        }
        encoder.finish().unwrap()
    }

    fn append_tar_entry(builder: &mut Builder<&mut GzEncoder<Vec<u8>>>, path: &str, bytes: &[u8]) {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, bytes).unwrap();
    }

    fn spawn_http_server(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });

        (format!("http://{addr}/codeql.tgz"), handle)
    }

    fn spawn_http_server_no_content_length(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n").unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });

        (format!("http://{addr}/codeql.tgz"), handle)
    }

    fn spawn_counting_http_server(
        body: Vec<u8>,
        request_count: Arc<AtomicUsize>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                request_count.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
                stream.flush().unwrap();
            }
        });

        (format!("http://{addr}/codeql.tgz"), handle)
    }

    fn spawn_truncated_http_server(
        body: Vec<u8>,
        advertised_len: usize,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {advertised_len}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });

        (format!("http://{addr}/codeql.tgz"), handle)
    }

    #[test]
    fn deploy_bundled_queries_creates_missing_query_files() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        fs::create_dir_all(&bundle_dir).unwrap();

        deploy_bundled_queries(&bundle_dir).unwrap();

        for (filename, expected_content) in BUNDLED_QUERIES {
            let path = bundle_dir
                .join("queries")
                .join(bundled_query_lang_dir(filename))
                .join(filename);
            assert!(path.exists(), "{filename} should be created");
            assert_eq!(fs::read_to_string(&path).unwrap(), *expected_content);
        }
        for (lang_dir, expected_content) in BUNDLED_QLPACKS {
            let path = bundle_dir.join("queries").join(lang_dir).join("qlpack.yml");
            assert!(path.exists(), "{lang_dir}/qlpack.yml should be created");
            assert_eq!(fs::read_to_string(&path).unwrap(), *expected_content);
        }
    }

    #[test]
    fn deploy_bundled_queries_overwrites_stale_content() {
        let temp = TempDir::new().unwrap();
        let bundle_dir = temp.path().join("codeql").join("2.0.0");
        let query_dir = bundle_dir.join("queries").join("rust");
        fs::create_dir_all(&query_dir).unwrap();
        fs::write(query_dir.join("extract-rust.ql"), "select 1\n").unwrap();

        deploy_bundled_queries(&bundle_dir).unwrap();

        let content = fs::read_to_string(query_dir.join("extract-rust.ql")).unwrap();
        assert_ne!(
            content, "select 1\n",
            "stale query should be overwritten with bundled content"
        );
        assert!(
            content.contains("query predicate"),
            "overwritten query should contain real predicates"
        );
    }

    #[test]
    fn bundled_queries_use_named_predicates_instead_of_select_stubs() {
        for (filename, query) in BUNDLED_QUERIES {
            assert!(
                !query.contains("select 1"),
                "{filename} should not contain a select stub"
            );
            for predicate in [
                "modules",
                "functions",
                "classes",
                "parameters",
                "variables",
                "contains",
                "calls",
                "control_flows",
                "data_flows",
            ] {
                assert!(
                    query.contains(&format!("query predicate {predicate}")),
                    "{filename} should define `{predicate}`"
                );
            }
        }
    }

    #[test]
    fn bundled_queries_emit_function_metric_support_data() {
        for (filename, query) in BUNDLED_QUERIES {
            for helper in ["parameterId", "variableId"] {
                assert!(
                    query.contains(&format!("private string {helper}")),
                    "{filename} should define `{helper}` so support nodes have stable ids"
                );
            }
            for edge in [
                "target = parameterId",
                "target = variableId",
                "source = parameterId",
                "source = variableId",
            ] {
                assert!(
                    query.contains(edge),
                    "{filename} should wire function metric support data through `{edge}`"
                );
            }
        }
    }

    #[test]
    fn bundled_python_query_uses_semantic_call_resolution() {
        let (_, query) = BUNDLED_QUERIES
            .iter()
            .find(|(filename, _)| *filename == "extract-python.ql")
            .expect("extract-python.ql should be bundled");

        assert!(
            !query.contains("call.getFunc().(Name).getId() = callee.getName()"),
            "Python bundled query should not resolve calls by name alone"
        );
        assert!(
            query.contains("FunctionValue"),
            "Python bundled query should use FunctionValue for call resolution"
        );
        assert!(
            query.contains("import semmle.python.objects.ObjectAPI"),
            "Python bundled query should import ObjectAPI to access FunctionValue"
        );
    }

    #[test]
    fn bundled_go_query_matches_bundled_codeql_pack_capabilities() {
        let (_, query) = BUNDLED_QUERIES
            .iter()
            .find(|(filename, _)| *filename == "extract-go.ql")
            .expect("extract-go.ql should be bundled");

        assert!(
            !query.contains("import codeql.files.FileSystem"),
            "Go bundled query should avoid unavailable FileSystem imports"
        );
        assert!(
            query.contains("FuncDecl"),
            "Go bundled query should use FuncDecl for file and location access"
        );
    }

    #[test]
    fn bundled_python_calls_predicate_uses_compilable_semantic_resolution() {
        let (_, query) = BUNDLED_QUERIES
            .iter()
            .find(|(filename, _)| *filename == "extract-python.ql")
            .expect("extract-python.ql should be bundled");

        assert!(
            query.contains("import semmle.python.objects.ObjectAPI"),
            "Python bundled query should import ObjectAPI for FunctionValue"
        );
        assert!(
            query.contains("FunctionValue"),
            "Python bundled query should resolve callees through FunctionValue"
        );
        assert!(
            !query.contains(".pointsTo(callee_value)"),
            "Python bundled query must not use Expr.pointsTo(FunctionValue) which fails \
             to compile under CodeQL 2.25.1 (RK0429/kalos#68)"
        );
        assert!(
            query.contains("callee_value.getACall() = call.getAFlowNode()"),
            "Python bundled query should resolve calls via \
             FunctionValue.getACall() = Call.getAFlowNode()"
        );
        assert!(
            !query.contains("call.getFunc().(Name).getId() = callee.getName()"),
            "Python bundled query must not regress to name-based call matching \
             (RK0429/kalos#48 false positives)"
        );
    }
}
