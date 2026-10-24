//! fzt, loaded from its library beside the executable: a menu session that takes key presses and
//! returns what to draw as data. The library is fzt-terminal's `cmd/fztlib`; every call passes
//! and returns JSON.

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::OnceLock;

#[cfg(windows)]
const LIBRARY: &str = "fzt.dll";
#[cfg(target_os = "macos")]
const LIBRARY: &str = "libfzt.dylib";
#[cfg(all(unix, not(target_os = "macos")))]
const LIBRARY: &str = "libfzt.so";

type OpenFn = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_char;
type SessionFn = unsafe extern "C" fn(c_int, *const c_char) -> *mut c_char;
type CloseFn = unsafe extern "C" fn(c_int);
type FreeFn = unsafe extern "C" fn(*mut c_char);

struct Api {
    // keeps the functions below loaded
    _library: libloading::Library,
    open: OpenFn,
    key: SessionFn,
    prompt: SessionFn,
    close: CloseFn,
    free: FreeFn,
}

fn api() -> Result<&'static Api> {
    static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| load().map_err(|err| format!("{err:#}")))
        .as_ref()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

fn load() -> Result<Api> {
    let path = std::env::current_exe()
        .context("find the executable")?
        .with_file_name(LIBRARY);
    // SAFETY: fzt's library has no initialization side effects, and each symbol is cast to the
    // signature cmd/fztlib exports it with.
    unsafe {
        let library =
            libloading::Library::new(&path).with_context(|| format!("load {}", path.display()))?;
        Ok(Api {
            open: *library.get::<OpenFn>(b"fzt_open\0")?,
            key: *library.get::<SessionFn>(b"fzt_key\0")?,
            prompt: *library.get::<SessionFn>(b"fzt_prompt\0")?,
            close: *library.get::<CloseFn>(b"fzt_close\0")?,
            free: *library.get::<FreeFn>(b"fzt_free\0")?,
            _library: library,
        })
    }
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    session: c_int,
    frame: Option<Frame>,
    #[serde(default)]
    error: String,
}

/// Read and free a reply from the library
fn reply(api: &Api, out: *mut c_char) -> Result<Response> {
    anyhow::ensure!(!out.is_null(), "fzt returned nothing");
    // SAFETY: the library returns a NUL-terminated string it allocated, freed with `free` once read
    let text = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    unsafe { (api.free)(out) };
    let response: Response = serde_json::from_str(&text).context("parse fzt's reply")?;
    if !response.error.is_empty() {
        anyhow::bail!("fzt: {}", response.error);
    }
    Ok(response)
}

fn frame(response: Response) -> Result<Frame> {
    response.frame.context("fzt replied without a frame")
}

fn json<T: Serialize>(value: &T) -> Result<CString> {
    Ok(CString::new(serde_json::to_string(value)?)?)
}

/// A session's options
#[derive(Serialize)]
pub struct Options {
    pub title: String,
    pub width: u16,
    pub height: u16,
    /// The app's name in the `:` palette
    pub frontend: String,
    /// The app's own commands in the `:` palette
    pub commands: Vec<Command>,
}

#[derive(Serialize)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub action: String,
}

/// A key press: a named key, or `char` with the character
#[derive(Serialize)]
pub struct KeyEvent {
    pub key: &'static str,
    pub char: String,
}

/// Switches the text box to take free text for the entry just picked
#[derive(Serialize)]
pub struct Prompt {
    pub mode: String,
    pub action: String,
    pub placeholder: String,
}

/// What to draw, and what the user picked once they have
#[derive(Deserialize, Debug, Default)]
pub struct Frame {
    #[serde(default)]
    pub rows: Vec<Row>,
    pub prompt: PromptState,
    pub result: Option<Pick>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub name: String,
    pub description: String,
    pub depth: usize,
    pub is_folder: bool,
    pub is_selected: bool,
    #[serde(default)]
    pub name_match_indices: Option<Vec<usize>>,
    #[serde(default)]
    pub desc_match_indices: Option<Vec<usize>>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptState {
    #[serde(default)]
    pub scope_path: Option<Vec<String>>,
    pub query: String,
    pub ghost: String,
    pub hint: String,
}

/// What the user picked: `select` (a menu entry, with its action), `prompt` (with the typed
/// text), `command` (one of the app's commands from the `:` palette) or `cancel`
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub kind: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub text: String,
}

/// An open menu, closed when dropped
#[derive(Debug)]
pub struct Session {
    id: c_int,
}

impl Session {
    /// Open a menu in fzt's YAML tree format
    pub fn open(menu: &str, options: &Options) -> Result<(Self, Frame)> {
        let api = api()?;
        let menu = CString::new(menu)?;
        let options = json(options)?;
        // SAFETY: both arguments are NUL-terminated strings that outlive the call
        let response = reply(api, unsafe { (api.open)(menu.as_ptr(), options.as_ptr()) })?;
        let session = Self {
            id: response.session,
        };
        Ok((session, frame(response)?))
    }

    pub fn key(&self, event: &KeyEvent) -> Result<Frame> {
        let api = api()?;
        let event = json(event)?;
        // SAFETY: the argument is a NUL-terminated string that outlives the call
        frame(reply(api, unsafe { (api.key)(self.id, event.as_ptr()) })?)
    }

    pub fn prompt(&self, prompt: &Prompt) -> Result<Frame> {
        let api = api()?;
        let prompt = json(prompt)?;
        // SAFETY: the argument is a NUL-terminated string that outlives the call
        frame(reply(api, unsafe {
            (api.prompt)(self.id, prompt.as_ptr())
        })?)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(api) = api() {
            // SAFETY: plain integer; closing an unknown session is a no-op
            unsafe { (api.close)(self.id) };
        }
    }
}
