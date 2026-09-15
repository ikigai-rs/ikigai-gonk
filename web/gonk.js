// gonk's only application script: error display for htmx, and the passkey ceremonies.
//
// Everything the page SHOWS comes from the server as HTML; this file does two things htmx
// cannot. (1) htmx does not swap a 4xx/5xx response, so a refused action would otherwise
// vanish silently — the error body is written into #flash as TEXT (never as HTML: an error
// can quote what a caller typed). (2) WebAuthn is a browser API; the ceremony is the
// standard one, with byte fields carried as base64url.
//
// ⚠ The session cookie is set HERE, not by the server — the HTTP transport cannot add a
// Set-Cookie header — so it is SameSite=Strict but not HttpOnly. The CSP forbids inline and
// third-party script, which is what that leaves as the defence.
(function () {
  "use strict";
  const $ = (id) => document.getElementById(id);
  const COOKIE = "gonk_session";

  function flash(text, kind) {
    const area = $("flash");
    if (!area) return;
    area.textContent = "";
    if (!text) return;
    const p = document.createElement("p");
    p.className = "flash " + (kind || "ok");
    p.setAttribute("role", kind === "error" ? "alert" : "status");
    p.textContent = text;
    area.appendChild(p);
  }

  document.addEventListener("htmx:responseError", (e) => {
    const xhr = e.detail.xhr;
    flash(((xhr && xhr.responseText) || "The server refused that.").trim(), "error");
  });
  document.addEventListener("htmx:sendError", () => flash("The server did not answer.", "error"));
  document.addEventListener("htmx:beforeRequest", () => flash("", "ok"));

  // ------------------------------------------------------------------ bytes

  function toB64(buffer) {
    const bytes = new Uint8Array(buffer);
    let s = "";
    for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  function fromB64(text) {
    const b = text.replace(/-/g, "+").replace(/_/g, "/");
    const raw = atob(b + "===".slice((b.length + 3) % 4));
    const out = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
    return out;
  }

  async function post(path, body) {
    const res = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: typeof body === "string" ? body : JSON.stringify(body || {}),
      credentials: "same-origin",
    });
    const text = await res.text();
    if (!res.ok) throw new Error(text.trim() || res.statusText);
    return text ? JSON.parse(text) : {};
  }

  function token() {
    const hit = document.cookie.split("; ").find((c) => c.startsWith(COOKIE + "="));
    return hit ? decodeURIComponent(hit.slice(COOKIE.length + 1)) : "";
  }

  function setToken(value, seconds) {
    document.cookie =
      COOKIE + "=" + encodeURIComponent(value) + "; Path=/; SameSite=Strict; Max-Age=" + seconds;
  }

  // ------------------------------------------------------------------ ceremonies

  // A WebAuthn call the browser refuses rejects with NotAllowedError whatever the cause — a
  // cancelled sheet, a timeout, or a page without focus — and its DOM text ("The document is
  // not focused", "The operation either timed out or was not allowed") says nothing a person
  // can act on. Say what to do instead. Anything else (a server refusal) is already words.
  function ceremonyError(err, retry) {
    if (err && err.name === "NotAllowedError") {
      return "the passkey prompt was cancelled, timed out, or opened while this window did not have focus. " + retry;
    }
    return (err && err.message) || String(err);
  }

  async function signIn() {
    try {
      const { challenge } = await post("/auth/login-options");
      const cred = await navigator.credentials.get({
        publicKey: {
          challenge: fromB64(challenge),
          rpId: "localhost",
          userVerification: "required",
          timeout: 120000,
        },
      });
      const r = cred.response;
      const done = await post("/auth/login", {
        challenge,
        id: cred.id,
        authenticatorData: toB64(r.authenticatorData),
        clientDataJSON: toB64(r.clientDataJSON),
        signature: toB64(r.signature),
      });
      setToken(done.session, done.seconds);
      location.reload();
    } catch (err) {
      flash("Sign-in did not complete: " + ceremonyError(err, "Click Sign in with passkey to try again."), "error");
      const login = $("auth-login");
      if (login && !login.hidden) login.focus();
    }
  }

  async function signOut() {
    const t = token();
    setToken("", 0);
    try {
      if (t) await post("/auth/logout", t);
    } catch (_) {
      // Signed out locally either way; the server session expires on its own.
    }
    location.reload();
  }

  async function enrol(invite, label) {
    try {
      const { challenge } = await post("/auth/register-options");
      const user = new Uint8Array(16);
      crypto.getRandomValues(user);
      const name = label || "gonk passkey";
      const cred = await navigator.credentials.create({
        publicKey: {
          challenge: fromB64(challenge),
          rp: { id: "localhost", name: "gonk" },
          user: { id: user, name, displayName: name },
          pubKeyCredParams: [{ type: "public-key", alg: -7 }],
          authenticatorSelection: { residentKey: "required", userVerification: "required" },
          attestation: "none",
          timeout: 120000,
        },
      });
      const key = cred.response.getPublicKey && cred.response.getPublicKey();
      if (!key) throw new Error("this browser does not expose the credential's public key");
      const done = await post("/auth/register", {
        challenge,
        invite,
        label: name,
        id: cred.id,
        publicKey: toB64(key),
        clientDataJSON: toB64(cred.response.clientDataJSON),
      });
      history.replaceState(null, "", location.pathname + location.search);
      $("enrol").hidden = true;
      // ★ Do NOT chain navigator.credentials.get() here. When create() resolves, the system
      // passkey sheet still holds focus, and Chrome refuses get() from an unfocused document
      // ("The document is not focused") — found by the first real Touch ID enrolment. A click
      // on the sign-in button is a fresh user gesture on a page that has focus back.
      const created = "Passkey created for " + done.label + " (grant " + done.grant + "). ";
      const login = $("auth-login");
      if ($("auth-logout").hidden) {
        login.hidden = false;
        flash(created + "Now click Sign in with passkey to use it.", "ok");
        login.focus();
      } else {
        flash(created + "Sign out, then sign in with the new passkey to use its grant.", "ok");
      }
    } catch (err) {
      flash(
        "Could not create the passkey: " +
          ceremonyError(err, "The invite is still good until it expires — click Create passkey to try again."),
        "error"
      );
    }
  }

  async function init() {
    const login = $("auth-login");
    if (!login) return; // a fragment, not a page
    if (location.hostname !== "localhost") {
      const note = $("auth-localhost");
      note.href = location.href.replace(location.hostname, "localhost");
      note.hidden = false;
      return;
    }
    if (!window.PublicKeyCredential) {
      $("auth-localhost").textContent = "This browser has no passkey support";
      $("auth-localhost").hidden = false;
      return;
    }
    login.addEventListener("click", signIn);
    $("auth-logout").addEventListener("click", signOut);

    const t = token();
    if (t) {
      try {
        const who = await post("/auth/session", t);
        $("auth-who").textContent = "Signed in as " + who.label + " (grant " + who.grant + ")";
        $("auth-who").hidden = false;
        $("auth-logout").hidden = false;
      } catch (_) {
        setToken("", 0);
        login.hidden = false;
      }
    } else {
      login.hidden = false;
    }

    const m = location.hash.match(/^#invite=([A-Za-z0-9_-]+)$/);
    if (m) {
      $("enrol").hidden = false;
      $("enrol-form").addEventListener("submit", (e) => {
        e.preventDefault();
        enrol(m[1], $("enrol-label").value.trim());
      });
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
