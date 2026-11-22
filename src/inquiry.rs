//! The inquiry layers client: `ikigai-inquiry`'s views and actions, mounted behind inquiry tokens
//! (ledger [#3695](http://localhost:1062/l/default/item/3695)).
//!
//! ```toml
//! gonk.inquiry.owner = "brian"
//! ```
//!
//! One line names the OWNER whose layers the client works on (`inquiry-brian`,
//! `inquiry-spam-brian`, …, as [`ikigai_inquiry::Layers::for_owner`] names them). It reaches them
//! through the Layers page's door and as the Layers page's identity (`gonk.layers.view`,
//! [`crate::layers_view`]), so a line without that one is refused at start, as is an owner the
//! crate refuses. Without the line nothing is mounted, and the pages say how to configure it.
//!
//! ```toml
//! gonk.inquiry.layers = "inquiry=inquiry spam=inquiry-spam holding=inquiry-hold examples=inquiry-example intents=inquiry-intent"
//! ```
//!
//! A host whose layers were named explicitly, not as `for_owner` names them (the edge's are not
//! owner-suffixed), names all five on a second line ([`LAYERS_KEY`], ledger
//! [#3980](http://localhost:1062/l/default/item/3980)); [`parse_layers`] says what it refuses.
//!
//! ```text
//! /inquiry/view/name/{view}                       urn:iki:gonk:page:inquiry:view:name:{view}                     Source  the client's view, in gonk's chrome
//! /inquiry/view/name/{view}/action/name/{action}  urn:iki:gonk:inquiry:view:name:{view}:action:name:{action}  Sink    one action on `inquiry=<id>`, then the view again
//! ```
//!
//! `{view}` is `pending` or `spam` (`day=YYYY-MM-DD`, today by default), and `{action}` one of
//! the crate's [`Action`]s. Each view's buttons post under that view's own action base, which is
//! how an action knows the page to return to.
//!
//! # ★ gonk is only the host
//!
//! There is no inquiry logic here. The views, the actions, the moves and the intents are the
//! crate's ([`ikigai_inquiry::space`]); gonk routes a page to a view, a POST to an action, and
//! gates both with its own tokens. Inquiry text is a stranger's text: it reaches a page only
//! through the crate's escaping, and gonk keeps no copy of it (no cache, no file, no log line
//! with text, no ledger write).
//!
//! # ★ The client's door is the Layers page's, and nothing else in gonk can use it
//!
//! The client holds no authority of its own: each view and action works by sub-requests to
//! `urn:layer:{layer}:…`. Those resolve in a kernel of this module's own, whose only other space
//! is `DoorSpace` over the Layers page's confined door ([`crate::layers_view::confined`]), so
//! they reach the configured identity's door and nowhere else. That kernel is mounted into
//! nothing: no `urn:layer:` name resolves through gonk's hub from the socket, QUIC, a script or
//! the model, and neither do the client's `urn:iki:inquiry:` names. A write leaves only when
//! `urn:layers:me` says the identity is a PERSON (`PersonWrites`), so what an action writes is
//! stamped as that person, never as this server.
//!
//! # ★ Who may use it: a signed-in passkey holding the owner's inquiry tokens
//!
//! | use | token |
//! |---|---|
//! | both views | `urn:cap:inquiry:read:{owner}` |
//! | remove | `urn:cap:inquiry:triage:{owner}` |
//! | not-spam and flag-spam | `urn:cap:inquiry:triage:{owner}` and `urn:cap:inquiry:curate:{owner}` |
//! | confirm-spam | `urn:cap:inquiry:curate:{owner}` |
//! | block and unblock | `urn:cap:inquiry:block:{owner}` |
//! | respond | `urn:cap:inquiry:respond:{owner}` |
//!
//! This server mints them for NO anonymous caller and no role: a passkey's grant carries them
//! (`grants.json`), the way `urn:cap:annotate` carries the Queue. A caller without one is
//! Denied before anything reaches the layer host. An action is a `POST` and passes the door's
//! same-origin rule (`crate::doors::http_refusal`) like every other write, and another page's
//! navigation keeps none of these tokens (`crate::admit::FOREIGN_PAGE_KEEPS`).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Bindings, Capability, Description, Endpoint, EndpointSpace, Error, Expiry, Fallback,
    Invocation, Iri, Kernel, Representation, Request, Resolution, Resolved, Result, Scope, Space,
    SystemClock, UriTemplate, Verb,
};
use ikigai_inquiry::{Action, Layers, Mount, View};

use crate::layer::Door;
use crate::render::{self, envelope};
use crate::web::{self, Web};

/// The config key.
pub const KEY: &str = "gonk.inquiry.owner";
/// The optional key naming the owner's five layers explicitly (ledger #3980).
pub const LAYERS_KEY: &str = "gonk.inquiry.layers";
/// The keys a [`LAYERS_KEY`] line names, every one of them, in the crate's field order.
pub const LAYER_KEYS: [&str; 5] = ["inquiry", "spam", "holding", "examples", "intents"];

/// A view's page.
pub const VIEW_TEMPLATE: &str = "urn:iki:gonk:page:inquiry:view:name:{view}";
/// Its path.
pub const VIEW_PATH: &str = "/inquiry/view/name/{view}";
/// An action, posted from a view's button.
pub const ACTION_TEMPLATE: &str = "urn:iki:gonk:inquiry:view:name:{view}:action:name:{action}";
/// Its path: the view's action base, then the action's name.
pub const ACTION_PATH: &str = "/inquiry/view/name/{view}/action/name/{action}";
/// Everything this module serves lies under it.
pub const PATH: &str = "/inquiry";
/// The pending view, where the header's link goes.
pub const PENDING_PATH: &str = "/inquiry/view/name/pending";
/// The day's spam.
pub const SPAM_PATH: &str = "/inquiry/view/name/spam";

/// The slot a view's body goes into.
const BODY_SLOT: &str = "inquiry";

/// One of the owner's inquiry tokens, `urn:cap:inquiry:{word}:{owner}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    /// Both views.
    Read,
    /// Taking an inquiry out of spam, or moving one between spam and inquiry.
    Triage,
    /// What trains the filter or keeps an example: moving one, or confirming spam.
    Curate,
    /// Asking the edge to block or unblock a sender.
    Block,
    /// Marking an inquiry handled.
    Respond,
}

impl Token {
    /// The token's word.
    pub fn word(self) -> &'static str {
        match self {
            Token::Read => "read",
            Token::Triage => "triage",
            Token::Curate => "curate",
            Token::Block => "block",
            Token::Respond => "respond",
        }
    }

    /// `urn:cap:inquiry:{word}:{owner}`.
    ///
    /// ```
    /// use ikigai_gonk::inquiry::Token;
    /// assert_eq!(Token::Read.iri("brian"), "urn:cap:inquiry:read:brian");
    /// ```
    pub fn iri(self, owner: &str) -> String {
        format!("urn:cap:inquiry:{}:{owner}", self.word())
    }

    fn purpose(self) -> &'static str {
        match self {
            Token::Read => "to read the pending inquiries and the day's spam",
            Token::Triage => "to take an inquiry out of spam, or move one between spam and inquiry",
            Token::Curate => "to decide what trains the filter: a move, or confirming spam",
            Token::Block => "to ask the edge to block or unblock a sender",
            Token::Respond => "to mark an inquiry handled",
        }
    }
}

/// The tokens `action` needs, every one of them.
///
/// ```
/// use ikigai_gonk::inquiry::{needs, Token};
/// use ikigai_inquiry::Action;
/// assert_eq!(needs(Action::NotSpam), [Token::Triage, Token::Curate]);
/// assert_eq!(needs(Action::Unblock), [Token::Block]);
/// ```
pub fn needs(action: Action) -> &'static [Token] {
    match action {
        Action::Remove => &[Token::Triage],
        Action::NotSpam | Action::FlagSpam => &[Token::Triage, Token::Curate],
        Action::ConfirmSpam => &[Token::Curate],
        Action::Block | Action::Unblock => &[Token::Block],
        Action::Respond => &[Token::Respond],
    }
}

/// A view's name in the paths: the last segment of `urn:iki:inquiry:view:name:{name}`.
fn view_name(view: View) -> &'static str {
    match view {
        View::Pending => "pending",
        View::Spam => "spam",
    }
}

fn view_iri(view: View) -> &'static str {
    match view {
        View::Pending => ikigai_inquiry::PENDING_VIEW,
        View::Spam => ikigai_inquiry::SPAM_VIEW,
    }
}

fn view_of(name: &str) -> Option<View> {
    View::ALL.into_iter().find(|v| view_name(*v) == name)
}

fn action_of(name: &str) -> Option<Action> {
    Action::ALL.into_iter().find(|a| a.name() == name)
}

/// Where `view`'s buttons post: its page's path, then `action/name/`.
fn action_base(view: View) -> String {
    format!("{PATH}/view/name/{}/action/name/", view_name(view))
}

// ------------------------------------------------------------------------------ config

/// What the [`KEY`] and [`LAYERS_KEY`] lines configure: the owner, and its layers' names when
/// they are given explicitly (`None`: [`Layers::for_owner`] names them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The owner whose layers the client works on, and whose inquiry tokens gate it.
    pub owner: String,
    /// The five layer names from a [`LAYERS_KEY`] line, already [`Layers::checked`].
    pub layers: Option<Layers>,
}

impl Config {
    /// The layers the client reaches: the explicit ones, or the owner's conventional names.
    ///
    /// # Errors
    ///
    /// An owner or a name the crate refuses.
    pub fn layers(&self) -> std::result::Result<Layers, String> {
        match &self.layers {
            Some(layers) => layers
                .clone()
                .checked()
                .map_err(|e| format!("{LAYERS_KEY}: {}", detail(e))),
            None => Layers::for_owner(&self.owner)
                .map_err(|e| format!("{KEY} `{}`: {}", self.owner, detail(e))),
        }
    }
}

fn detail(e: Error) -> String {
    match e {
        Error::InvalidArgument { detail, .. } => detail,
        other => other.to_string(),
    }
}

/// Parse a [`LAYERS_KEY`] line: `key=name` pairs separated by whitespace, one for each of
/// [`LAYER_KEYS`].
///
/// ```
/// use ikigai_gonk::inquiry::parse_layers;
/// let layers = parse_layers(
///     "inquiry=inquiry spam=inquiry-spam holding=inquiry-hold \
///      examples=inquiry-example intents=inquiry-intent",
/// )
/// .unwrap();
/// assert_eq!(layers.inquiry, "inquiry");
/// assert_eq!(layers.intents, "inquiry-intent");
/// assert!(parse_layers("inquiry=inquiry").is_err());
/// ```
///
/// # Errors
///
/// A pair without `=`, an unknown key, a key given twice, a missing key, or a name
/// [`Layers::checked`] refuses; each sentence names the key.
pub fn parse_layers(line: &str) -> std::result::Result<Layers, String> {
    let mut names: [Option<String>; 5] = Default::default();
    for pair in line.split_whitespace() {
        let Some((key, name)) = pair.split_once('=') else {
            return Err(format!(
                "{LAYERS_KEY}: `{pair}` is not `key=name`; the keys are {}",
                LAYER_KEYS.join(", ")
            ));
        };
        let Some(at) = LAYER_KEYS.iter().position(|k| *k == key) else {
            return Err(format!(
                "{LAYERS_KEY}: `{key}` is not a layer key; the keys are {}",
                LAYER_KEYS.join(", ")
            ));
        };
        if names[at].is_some() {
            return Err(format!(
                "{LAYERS_KEY}: `{key}` is given twice: each layer has one name"
            ));
        }
        // Each name alone, so the refusal names the key it was given for.
        let one = Layers {
            inquiry: name.to_string(),
            spam: name.to_string(),
            holding: name.to_string(),
            examples: name.to_string(),
            intents: name.to_string(),
        };
        if let Err(e) = one.checked() {
            return Err(format!("{LAYERS_KEY}: `{key}`: {}", detail(e)));
        }
        names[at] = Some(name.to_string());
    }
    let missing: Vec<&str> = LAYER_KEYS
        .iter()
        .zip(&names)
        .filter(|(_, n)| n.is_none())
        .map(|(k, _)| *k)
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{LAYERS_KEY}: no `{}`: the line names all five layers ({})",
            missing.join("`, `"),
            LAYER_KEYS.join(", ")
        ));
    }
    let [Some(inquiry), Some(spam), Some(holding), Some(examples), Some(intents)] = names else {
        unreachable!("every key is present");
    };
    Layers {
        inquiry,
        spam,
        holding,
        examples,
        intents,
    }
    .checked()
    .map_err(|e| format!("{LAYERS_KEY}: {}", detail(e)))
}

/// Parse the [`KEY`] lines (none, or one naming an owner) and the [`LAYERS_KEY`] lines (none, or
/// one naming the owner's layers). `has_view` is whether a `gonk.layers.view` line gives the
/// door and identity the client reaches the layers through.
///
/// ```
/// use ikigai_gonk::inquiry::parse_config;
/// assert_eq!(parse_config(&[], &[], false).unwrap(), None);
/// let got = parse_config(&["brian".into()], &[], true).unwrap().unwrap();
/// assert_eq!((got.owner.as_str(), got.layers), ("brian", None));
/// assert!(parse_config(&["Brian".into()], &[], true).is_err());
/// assert!(parse_config(&["brian".into()], &[], false).is_err());
/// assert!(parse_config(&["a".into(), "b".into()], &[], true).is_err());
/// assert!(parse_config(&[], &["inquiry=inquiry".into()], true).is_err());
/// ```
///
/// # Errors
///
/// Either key given twice, an owner the crate refuses, a layers line [`parse_layers`] refuses or
/// one without an owner, or no `gonk.layers.view` to reach them by.
pub fn parse_config(
    lines: &[String],
    layer_lines: &[String],
    has_view: bool,
) -> std::result::Result<Option<Config>, String> {
    let layers = match layer_lines {
        [] => None,
        [line] => Some(parse_layers(line)?),
        _ => {
            return Err(format!(
                "{LAYERS_KEY} is given {} times: one line names all five layers",
                layer_lines.len()
            ))
        }
    };
    let line = match lines {
        [] if layers.is_some() => {
            return Err(format!(
                "{LAYERS_KEY} is set and no {KEY} line is: the layers are the owner's, whose \
                 inquiry tokens gate the client"
            ))
        }
        [] => return Ok(None),
        [line] => line.trim(),
        _ => {
            return Err(format!(
                "{KEY} is given {} times: this server mounts ONE owner's inquiry client",
                lines.len()
            ))
        }
    };
    if let Err(e) = Layers::for_owner(line) {
        return Err(format!("{KEY} `{line}`: {}", detail(e)));
    }
    if !has_view {
        return Err(format!(
            "{KEY} is set and no {} line is: the inquiry client reaches the owner's layers \
             through the Layers page's door, as the identity that line names",
            crate::layers_view::KEY
        ));
    }
    Ok(Some(Config {
        owner: line.to_string(),
        layers,
    }))
}

// ------------------------------------------------------------------------------ the door

/// The client's `urn:layer:`, `urn:layers:` and `urn:collab:` sub-requests, sent to the door and
/// nowhere else. Every other name misses here.
struct DoorSpace(Arc<dyn Door>);

impl Space for DoorSpace {
    fn resolve(&self, request: &Request, _scope: &Scope) -> Resolution {
        let target = request.target.as_str();
        if !crate::layers_view::PREFIXES
            .iter()
            .any(|p| target.starts_with(p))
        {
            return Resolution::Miss;
        }
        Resolution::Hit(Resolved::new(
            Arc::new(DoorCall(Arc::clone(&self.0))),
            Bindings::default(),
        ))
    }
}

/// One sub-request at the door, off the async thread (a door call blocks).
struct DoorCall(Arc<dyn Door>);

#[async_trait]
impl Endpoint for DoorCall {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let door = Arc::clone(&self.0);
        let request = inv.request.clone();
        let answer = crate::layers_view::off_thread(move || door.call(request)).await??;
        // A layer changes under every action: nothing it answers is kept, here or upstream.
        Ok(answer.with_expiry(Expiry::Always))
    }

    fn name(&self) -> &str {
        "gonk-inquiry-door"
    }
}

/// A door that writes only once `urn:layers:me` says the identity is a person.
struct PersonWrites {
    door: Arc<dyn Door>,
    /// Set once the door said `person`; the identity is the certificate's, fixed while gonk runs.
    person: OnceLock<()>,
}

impl PersonWrites {
    fn check(&self) -> std::result::Result<(), Error> {
        if self.person.get().is_some() {
            return Ok(());
        }
        let me = self.door.call(
            Request::new(
                Verb::Source,
                Iri::parse("urn:layers:me").expect("a fixed IRI"),
            )
            .with_arg("as", ArgRef::Inline(b"application/json".to_vec())),
        )?;
        let me = crate::layers_view::json(&me)
            .map_err(|e| Error::Endpoint(format!("urn:layers:me: {e}")))?;
        match me["kind"].as_str() {
            Some("person") => {
                let _ = self.person.set(());
                Ok(())
            }
            kind => Err(Error::Denied(format!(
                "the door says the identity the inquiry client is configured with is {}, not a \
                 person; the client writes only as a person",
                kind.unwrap_or("of no kind it names")
            ))),
        }
    }
}

impl Door for PersonWrites {
    fn call(&self, request: Request) -> std::result::Result<Representation, Error> {
        if request.verb != Verb::Source {
            self.check()?;
        }
        self.door.call(request)
    }
}

// ------------------------------------------------------------------------------ the client

/// One owner's client: a kernel per view, each binding the crate's client (that view, its
/// actions posting under that view's base) over the same confined door.
pub struct Inquiry {
    owner: String,
    pending: Kernel,
    spam: Kernel,
}

impl Inquiry {
    /// The client for `owner`, reaching its layers through `door`: the Layers page's door in
    /// production ([`crate::layers_view::dial`]), which is confined here again to
    /// [`crate::layers_view::PREFIXES`] and writes only as a person.
    ///
    /// # Errors
    ///
    /// An owner the crate refuses.
    pub fn new(owner: &str, door: Arc<dyn Door>) -> std::result::Result<Arc<Inquiry>, String> {
        Inquiry::configured(
            &Config {
                owner: owner.to_string(),
                layers: None,
            },
            door,
        )
    }

    /// The client [`Config`] describes: the owner's tokens, and its explicit layers when a
    /// [`LAYERS_KEY`] line named them (`for_owner`'s names otherwise), through `door` as
    /// [`Inquiry::new`] reaches them.
    ///
    /// # Errors
    ///
    /// An owner or a layer name the crate refuses.
    pub fn configured(
        config: &Config,
        door: Arc<dyn Door>,
    ) -> std::result::Result<Arc<Inquiry>, String> {
        let owner = config.owner.as_str();
        let layers = config.layers()?;
        let door = crate::layers_view::confined(Arc::new(PersonWrites {
            door,
            person: OnceLock::new(),
        }));
        let kernel = |view: View| {
            let client = ikigai_inquiry::space(
                Mount::new(layers.clone())
                    .views([view])
                    .action_base(action_base(view)),
            );
            let spaces: Vec<Arc<dyn Space>> =
                vec![Arc::new(client), Arc::new(DoorSpace(Arc::clone(&door)))];
            Kernel::new(Arc::new(Fallback::new(spaces))).with_clock(Arc::new(SystemClock))
        };
        Ok(Arc::new(Inquiry {
            owner: owner.to_string(),
            pending: kernel(View::Pending),
            spam: kernel(View::Spam),
        }))
    }

    /// The owner whose layers this client works on.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    fn kernel(&self, view: View) -> &Kernel {
        match view {
            View::Pending => &self.pending,
            View::Spam => &self.spam,
        }
    }

    /// Refuse `capability` unless it holds every one of `tokens`, naming the missing ones.
    fn require(&self, capability: &Capability, tokens: &[Token], what: &str) -> Result<()> {
        let missing: Vec<Token> = tokens
            .iter()
            .copied()
            .filter(|t| !capability.allows(&t.iri(&self.owner)))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let lacking = web::Lacking {
            signed_out: format!("Sign in to {what}."),
            signed_in: format!("Your passkey's grant doesn't let you {what}."),
            needs: missing
                .iter()
                .map(|t| (t.iri(&self.owner), t.purpose().to_string()))
                .collect(),
        };
        Err(Error::Denied(
            lacking.plain(capability.principal().is_some()),
        ))
    }

    /// The crate's view page, as the caller's authority reads it.
    async fn view(&self, view: View, day: Option<&str>, capability: &Capability) -> Result<String> {
        let mut request = Request::new(
            Verb::Source,
            Iri::parse(view_iri(view)).expect("a fixed IRI"),
        );
        if let Some(day) = day {
            request = request.with_arg("day", ArgRef::Inline(day.as_bytes().to_vec()));
        }
        let answer = self.kernel(view).issue(request, capability).await?;
        String::from_utf8(answer.bytes)
            .map_err(|_| Error::Endpoint("the inquiry view did not answer UTF-8".to_string()))
    }

    /// One action on `id`, through the view's own client; the crate's sentence.
    async fn act(
        &self,
        view: View,
        action: Action,
        id: &str,
        capability: &Capability,
    ) -> Result<String> {
        let request = Request::new(
            Verb::Sink,
            Iri::parse(action.iri()).expect("the crate's own IRI"),
        )
        .with_arg("inquiry", ArgRef::Inline(id.as_bytes().to_vec()));
        let answer = self.kernel(view).issue(request, capability).await?;
        Ok(String::from_utf8_lossy(&answer.bytes).trim().to_string())
    }
}

// ------------------------------------------------------------------------------ the pages

/// The crate's page, taken apart: its `h1`'s words and what follows it inside `main`.
fn parts(page: &str) -> Result<(String, String)> {
    let malformed = || Error::Endpoint("the inquiry view answered no page this host reads".into());
    let open = page.find("<main>").ok_or_else(malformed)?;
    let rest = &page[open + "<main>".len()..];
    let h1 = rest.find("<h1>").ok_or_else(malformed)?;
    let h1_end = rest.find("</h1>").ok_or_else(malformed)?;
    let close = rest.rfind("</main>").ok_or_else(malformed)?;
    if !(h1 < h1_end && h1_end < close) {
        return Err(malformed());
    }
    let title = unescape(&rest[h1 + "<h1>".len()..h1_end]);
    let body = rest[h1_end + "</h1>".len()..close].trim().to_string();
    Ok((title, body))
}

/// The crate's own title back to words (it escapes with `ikigai_fn::html::html_text`); the
/// envelope escapes them again.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

/// Whether `day` is `YYYY-MM-DD` in form (the crate checks it is one before it reads a layer).
fn is_day(day: &str) -> bool {
    let b = day.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// The day a spam view shows, read back from its title (`Spam for YYYY-MM-DD`).
fn day_of(title: &str) -> Option<&str> {
    title.strip_prefix("Spam for ").filter(|d| is_day(d))
}

/// The spam view's buttons, posting `day` along, so an action on another day's spam comes back
/// to that day: each `action="{base}{name}"` the crate drew gains `?day={day}`. Only the
/// attribute the crate writes from the mount's base matches; visitor text reaches the body
/// escaped, so none of it can.
fn with_day(body: String, day: &str) -> String {
    if !is_day(day) {
        return body;
    }
    let base = action_base(View::Spam);
    Action::ALL.into_iter().fold(body, |body, action| {
        let target = format!("action=\"{base}{}\"", action.name());
        body.replace(
            &target,
            &format!("action=\"{base}{}?day={day}\"", action.name()),
        )
    })
}

/// What a page says in its status region after an action.
struct Status {
    ok: bool,
    message: String,
}

/// A view in gonk's chrome: the header, the theme, the view's tabs, `status` in the page's
/// status region, and `body` (the crate's) spliced into the slot.
fn page(
    web: &Web,
    inv: &Invocation<'_>,
    view: View,
    title: &str,
    body: Option<String>,
    message: &str,
    status: Option<&Status>,
) -> Result<Representation> {
    let ledgers = web::readable_ledgers(web, inv.capability);
    // The header's link goes to the pending view, so it is the current page only there.
    let mut children = match view {
        View::Pending => web::nav_on_inquiry(web, inv.capability, &ledgers),
        View::Spam => web::nav(web, inv.capability, &ledgers, None),
    };
    if body.is_some() {
        children.push_str(&render::slot(BODY_SLOT));
    }
    let mut attributes = vec![
        ("view", "inquiry".to_string()),
        ("full", "true".to_string()),
        ("ui-theme", render::theme_of(inv).to_string()),
        (
            "safe",
            crate::page_assets::flag(crate::page_assets::safe_of(inv)).to_string(),
        ),
        ("title", title.to_string()),
        ("inquiry-view", view_name(view).to_string()),
        ("pending-href", PENDING_PATH.to_string()),
        ("spam-href", SPAM_PATH.to_string()),
    ];
    if !message.is_empty() {
        attributes.push(("message", message.to_string()));
    }
    if body.is_some() {
        if let Some(day) = day_of(title) {
            attributes.push(("day", day.to_string()));
        }
    } else {
        attributes.push(("config", format!("{KEY} = \"<owner>\"")));
    }
    if let Some(status) = status {
        attributes.push(("status-message", status.message.clone()));
        attributes.push((
            "status-kind",
            if status.ok { "ok" } else { "error" }.to_string(),
        ));
    }
    let attributes: Vec<(&str, &str)> = attributes.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let doc = envelope("page", &attributes, &children);
    let shell = render::render(&doc, true).map_err(web::render_err)?;
    let html = render::splice(shell, &[(BODY_SLOT.to_string(), body.unwrap_or_default())])
        .map_err(web::render_err)?;
    Ok(web::html(html).with_expiry(Expiry::Always))
}

/// The page an unconfigured server answers at every view: what is missing and the line to add.
fn not_configured(web: &Web, inv: &Invocation<'_>, view: View) -> Result<Representation> {
    page(
        web,
        inv,
        view,
        "Inquiries",
        None,
        &format!(
            "The inquiry client is not configured on this server. Add one line to config.toml \
             and restart gonk: the owner whose inquiry layers it shows. It reaches them through \
             the Layers page's door, as the identity the {} line names.",
            crate::layers_view::KEY
        ),
        None,
    )
}

fn view_binding(inv: &Invocation<'_>) -> Result<View> {
    let name = web::binding(inv, "view")?;
    view_of(&name).ok_or_else(|| {
        Error::NotFound(format!(
            "no inquiry view `{name}`: the views are pending and spam"
        ))
    })
}

fn optional_arg(inv: &Invocation<'_>, name: &str) -> Option<String> {
    inv.inline_str(name)
        .ok()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Bind the client's pages into the HTTP face's page space. `None` binds them all the same, and
/// they say the client is not configured.
pub fn bind(space: EndpointSpace, web: &Arc<Web>, inquiry: Option<Arc<Inquiry>>) -> EndpointSpace {
    let template = |t: &str| UriTemplate::parse(t).expect("a constant template");
    space
        .bind(
            template(VIEW_TEMPLATE),
            ViewPage {
                web: Arc::clone(web),
                inquiry: inquiry.clone(),
            },
        )
        .bind(
            template(ACTION_TEMPLATE),
            ActionPost {
                web: Arc::clone(web),
                inquiry,
            },
        )
}

/// `urn:iki:gonk:page:inquiry:view:name:{view}`.
struct ViewPage {
    web: Arc<Web>,
    inquiry: Option<Arc<Inquiry>>,
}

#[async_trait]
impl Endpoint for ViewPage {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Endpoint(
                "an inquiry view answers Source only".to_string(),
            ));
        }
        web::html_only(inv)?;
        let view = view_binding(inv)?;
        let Some(inquiry) = &self.inquiry else {
            return not_configured(&self.web, inv, view);
        };
        inquiry.require(inv.capability, &[Token::Read], "read the inquiries")?;
        let day = match view {
            View::Spam => optional_arg(inv, "day"),
            View::Pending => None,
        };
        let html = inquiry.view(view, day.as_deref(), inv.capability).await?;
        let (title, body) = parts(&html)?;
        let body = match day_of(&title) {
            Some(day) => with_day(body, day),
            None => body,
        };
        page(&self.web, inv, view, &title, Some(body), "", None)
    }

    fn name(&self) -> &str {
        "gonk-inquiry-view"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-inquiry-view")
            .title("Inquiries")
            .summary(
                "The inquiry client's view (`pending`, or `spam` with an optional `day`) in \
                 gonk's chrome, read through the Layers page's door. Needs \
                 urn:cap:inquiry:read:{owner}.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(web::arg("view", "pending or spam").binding())
            .input(web::arg("day", "the spam view's day, YYYY-MM-DD; today by default").optional())
            .input(web::as_html_arg())
            .output("text/html")
    }
}

/// `urn:iki:gonk:inquiry:view:name:{view}:action:name:{action}`.
struct ActionPost {
    web: Arc<Web>,
    inquiry: Option<Arc<Inquiry>>,
}

#[async_trait]
impl Endpoint for ActionPost {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Endpoint(
                "an inquiry action answers Sink only".to_string(),
            ));
        }
        let view = view_binding(inv)?;
        let name = web::binding(inv, "action")?;
        let action = action_of(&name)
            .ok_or_else(|| Error::NotFound(format!("no inquiry action `{name}`")))?;
        let Some(inquiry) = &self.inquiry else {
            return Err(Error::NotFound(format!(
                "the inquiry client is not configured on this server: add a {KEY} line"
            )));
        };
        inquiry.require(
            inv.capability,
            needs(action),
            &format!("{} an inquiry", action.name()),
        )?;
        let fields = web::form(inv.inline_str("content").unwrap_or_default());
        let id = fields
            .get("inquiry")
            .map(String::as_str)
            .unwrap_or_default();
        let status = match inquiry.act(view, action, id, inv.capability).await {
            Ok(message) => Status { ok: true, message },
            // Another person, or another tab, took it first: the crate names who.
            Err(Error::Conflict(message)) | Err(Error::NotFound(message)) => {
                Status { ok: false, message }
            }
            Err(e) => return Err(e),
        };
        // Back to the view the button was on: the spam view's buttons carry its day.
        let day = match view {
            View::Spam => optional_arg(inv, "day").filter(|d| is_day(d)),
            View::Pending => None,
        };
        let html = inquiry.view(view, day.as_deref(), inv.capability).await?;
        let (title, body) = parts(&html)?;
        let body = match day_of(&title) {
            Some(day) => with_day(body, day),
            None => body,
        };
        page(&self.web, inv, view, &title, Some(body), "", Some(&status))
    }

    fn name(&self) -> &str {
        "gonk-inquiry-action"
    }

    fn describe(&self) -> Description {
        Description::new("gonk-inquiry-action")
            .title("Act on an inquiry")
            .summary(
                "A view's button (the form's `inquiry` field) → the inquiry client's action, \
                 under the caller's inquiry tokens, then the view again with the action's \
                 sentence, or its Conflict, in the status region.",
            )
            .verb(Verb::Sink)
            .verb(Verb::Meta)
            .input(web::arg("view", "the view the button was on: pending or spam").binding())
            .input(web::arg("action", "the action's name").binding())
            .input(web::arg("content", "the form, url-encoded: inquiry"))
            .input(
                web::arg(
                    "day",
                    "the spam view's day to return to, YYYY-MM-DD; today by default",
                )
                .optional(),
            )
            .output("text/html")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_taken_apart_at_its_main() {
        let page =
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<title>Spam for 2026-11-16</title>\n\
                    </head>\n<body>\n<main>\n<h1>Spam for 2026-11-16</h1>\n<p>x &amp; y</p>\n\
                    </main>\n</body>\n</html>\n";
        let (title, body) = parts(page).unwrap();
        assert_eq!(title, "Spam for 2026-11-16");
        assert_eq!(body, "<p>x &amp; y</p>");
        assert_eq!(day_of(&title), Some("2026-11-16"));
        assert!(parts("<p>no main</p>").is_err());
        assert_eq!(day_of("Spam for 2026/11/16"), None);
    }

    #[test]
    fn the_spam_buttons_carry_their_day_and_nothing_else_changes() {
        let body = "<form method=\"post\" action=\"/inquiry/view/name/spam/action/name/remove\">\
                    <p>action=&quot;/inquiry/view/name/spam/action/name/remove&quot;</p>"
            .to_string();
        let got = with_day(body.clone(), "2026-11-14");
        assert!(
            got.contains("action=\"/inquiry/view/name/spam/action/name/remove?day=2026-11-14\"")
        );
        assert!(
            got.contains("<p>action=&quot;/inquiry/view/name/spam/action/name/remove&quot;</p>")
        );
        assert_eq!(with_day(body.clone(), "x\" onclick=\"y"), body);
    }

    #[test]
    fn every_action_needs_a_token_and_every_view_has_a_base() {
        for action in Action::ALL {
            assert!(!needs(action).is_empty(), "{action:?}");
            assert!(action_of(action.name()) == Some(action));
        }
        assert_eq!(
            action_base(View::Spam),
            "/inquiry/view/name/spam/action/name/"
        );
        for view in View::ALL {
            assert_eq!(view_of(view_name(view)), Some(view));
        }
    }
}
