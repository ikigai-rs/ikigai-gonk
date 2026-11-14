// gonk's own page behavior, and the passkey ceremonies. Refusals, the status region, focus after a
// swap and the theme are ikigai-ui's shell script (`urn:iki:ui:shell.js`, /static/shell.js, ledger
// #2834), which loads first; a poll that retries on its own says so with `data-ui-quiet` in the
// markup, so its failures are never messages.
//
// Everything the page SHOWS comes from the server as HTML; this file does what htmx cannot. (2)
// WebAuthn is a browser API; the ceremony is the
// standard one, with byte fields carried as base64url. (3) the browse family's affordances
// name the /k/ adapter with a COMMAND in the path, which gonk's door cannot parse back into
// a resource — so the command is folded into a query value here, one line, grammar-level.
// (4) one fragment tells another there is news, because gonk cannot send an `HX-Trigger`
// response header — and it holds that news back while a human is mid-decision, which is a
// question about the DOM that only the browser can answer (ledger #469). (5) a batch
// finding's word picker is required exactly while its box is ticked and no batch word stands
// behind it — a condition over three controls, which no static attribute can state (ledger
// #657). (6) a line selected by its number in a browse file view fills that view's annotate
// quote — browse ships no scripts, and the selection is browse's own  (ledger #658) — and the
// form gets a line field, the keyboard's way to the same fill (ledger #2905). (7) a
// page a caller's grant does not reach says "sign in" to a signed-out visitor and "your grant
// doesn't include it" to a signed-in one, and which of the two is a question about the
// session that only this script asks of the server on a read (ledger #739). (8) the Layers page's
// post form, replaced by its answer, gives focus back to its text box (ledger #1675). (9) the
// Layers page's poll, which htmx would leave dead after one failed ask, is re-armed with a
// backoff and its state said in words (ledger #1705). (10) the header's ledger menu, a
// <details> without this file, becomes a disclosure button that Escape and a click outside
// close, with a filter that narrows its links past eight ledgers (ledger #1888). (11) a browse
// page's "On GitHub" link follows the region to the view it now shows, which htmx swaps in
// without changing the page's URL, and carries a selected line onto it (ledger #1907, and a
// pull request's page, #2117). (12) a page asks the server, now and then, whether it is still
// the process that served it, and says so when it is not (ledger #375).
//
// ⚠ The session cookie is set HERE, not by the server — the HTTP transport cannot add a
// Set-Cookie header — so it is SameSite=Strict but not HttpOnly. The CSP forbids inline and
// third-party script, which is what that leaves as the defense.
(function () {
  "use strict";
  const $ = (id) => document.getElementById(id);
  const COOKIE = "gonk_session";

  // A sentence of gonk's own (a sample loaded, a passkey made), in the page's one status region
  // (D13). A failure with no control to sit beside is an alert; anything else the region's
  // politeness says.
  function flash(text, kind) {
    const area = $("status");
    if (!area) return;
    area.textContent = "";
    if (!text) return;
    const p = document.createElement("p");
    p.className = "flash " + (kind || "ok");
    if (kind === "error") p.setAttribute("role", "alert");
    p.textContent = text;
    area.appendChild(p);
  }

  // A sentence already on screen somewhere else, said once in the status region for a screen
  // reader (D13: a note is plain text, and #status is the one region that announces). It joins
  // what the region holds, so a visible message there is not taken away.
  function announce(text) {
    const area = $("status");
    if (!area || !text) return null;
    const p = document.createElement("p");
    p.className = "ui-sr-only";
    p.textContent = text;
    area.appendChild(p);
    return p;
  }

  // Whole-match counts arrive independently. The fallback link lives outside the swap,
  // so arrival cannot remove a focused control. Failures leave the list and link usable.
  document.addEventListener("htmx:afterSwap", (e) => {
    const target = e.detail && e.detail.target;
    if (target && target.id === "listing-facet") announce("Labels and priority counts loaded.");
  });
  function facetFailure(e) {
    const elt = e.detail && e.detail.elt;
    if (!elt || !elt.isConnected || elt.id !== "listing-facet") return;
    const sentence = "Counts could not load. Open the counts link or reload to try again.";
    elt.textContent = sentence;
    announce(sentence);
  }
  document.addEventListener("htmx:responseError", facetFailure);
  document.addEventListener("htmx:sendError", facetFailure);

  // The Layers page's watcher (ledger #1705): its failures are its own, said once in
  // `#layers-reconnect` (section 9 below). `data-ui-quiet` keeps the shell script out of them.
  function isWatch(elt) {
    return !!(elt && elt.closest && elt.closest(".layers-watch"));
  }

  // The browse page's "On GitHub" link (section 11 below) is fetched by nobody's click: a
  // failure hides the link, it is not a message.
  function isGithub(elt) {
    return !!(elt && elt.closest && elt.closest("#github"));
  }

  // The Queue's refresh is quiet too: it swaps into itself, and the shell script's note would
  // take the place of the findings a person is reading. Its failure is a sentence in #status.
  function isQueueRefresh(elt) {
    return !!(elt && elt.id === "queue");
  }
  document.addEventListener("htmx:responseError", (e) => {
    if (!isQueueRefresh(e.detail && e.detail.elt)) return;
    const xhr = e.detail.xhr;
    flash(((xhr && xhr.responseText) || "The server refused that.").trim(), "error");
  });
  document.addEventListener("htmx:sendError", (e) => {
    if (isQueueRefresh(e.detail && e.detail.elt)) flash("The server did not answer.", "error");
  });

  // ---------------------------------------------- the /k/ adapter's path spelling
  //
  // ikigai-browse authors every affordance as hx-get="/k/source {iri} [k=v ...]" or
  // hx-post="/k/sink urn:iki:annotation" — a COMMAND in the path, with spaces in it, and
  // with slashes inside the IRI. gonk's HTTP door percent-decodes the path and then rebuilds
  // a target IRI from it, so such a path is a 400 before any of gonk's code sees it: a space
  // is not legal in an IRI. (ikigai-web's standalone server parses the raw request-target
  // itself, which is why the same affordances work at 8642 untouched.)
  //
  // So the command travels as one query value instead, and this is where the two spellings
  // meet. It is grammar-level and knows nothing about which button was pressed: any /k/
  // request, from any face, present or future, folds the same way. The server's own shell
  // already emits the query form, so a broken rewrite here cannot make the first paint fail
  // silently — it fails on the first affordance, loudly, beside it.
  document.addEventListener("htmx:configRequest", (e) => {
    const path = e.detail && e.detail.path;
    if (typeof path !== "string" || path.slice(0, 3) !== "/k/") return;
    e.detail.path = "/k?c=" + encodeURIComponent(path.slice(3));
  });

  // ------------------------------------------------- the queue's news, and who is mid-decision
  //
  // ★ THE FINDINGS LIST HAS NO CLOCK (ledger #469). The header badge already polls the
  // depth at the server's own interval; every answer carries `data-rev`, the queue's whole
  // state as one token. When that token changes there is something new to show, and this
  // relays it to the list as one event. When it does not change nothing is fetched, which
  // is the difference between this and a second `every 10s` on the list.
  //
  // ⚠ Why this is a script at all, when everything else the page shows comes from the
  // server: the badge cannot carry an `HX-Trigger` response header — gonk's HTTP transport
  // has no way to add one, which is the same limitation that puts the session cookie here —
  // and the badge must not know what a findings list looks like. So the DECISION is the
  // server's (it computes the revision) and this is the wire between two fragments.
  //
  // ⚠⚠ AND IT NEVER YANKS A ROW SOMEONE IS DECIDING ON. A swap that replaces the list while
  // a human is choosing a severity or typing a reason is worse than a stale page: it throws
  // away work they did. So news arriving during a decision is HELD — the page says so, out
  // loud — and delivered when the decision is submitted (which swaps the list anyway) or
  // abandoned.
  const NEWS = "gonk:news";
  let seenRev = null;
  let held = false;

  function queueSection() {
    return $("queue");
  }

  // Is a human in the middle of deciding? Focus is the obvious half; the other half is a
  // control they have already changed and not yet submitted, which survives losing focus.
  function midDecision(queue) {
    // ★ A form drawn back after a refusal (ledger #657) IS a decision in progress, though
    // nothing on it differs from what the server drew: what the server drew was the person's
    // own submission. A refresh would put the source's defaults back over it.
    if (queue.querySelector("form[data-refused]")) return true;
    const active = document.activeElement;
    if (active && queue.contains(active) && active.matches("select, textarea, input, button")) {
      return true;
    }
    const changed = (sel) => {
      const rendered = sel.querySelector("option[selected]");
      const initial = rendered ? rendered.value : sel.options.length ? sel.options[0].value : "";
      return sel.value !== initial;
    };
    if (Array.prototype.some.call(queue.querySelectorAll("select"), changed)) return true;
    // A box the person has unticked (or ticked) in a batch (ledger #506) is a decision in
    // progress too: a refresh would put every box back the way the server drew it.
    const flipped = (box) => box.checked !== box.defaultChecked;
    if (Array.prototype.some.call(queue.querySelectorAll("input[type=checkbox]"), flipped)) {
      return true;
    }
    return Array.prototype.some.call(queue.querySelectorAll("textarea"), (t) => t.value !== "");
  }

  function deliver() {
    const queue = queueSection();
    // Not on the Queue page: the badge still polls (it is in every header) and there is
    // simply nothing here to refresh.
    if (!queue) return;
    if (midDecision(queue)) {
      const notice = $("queue-stale");
      if (notice) {
        if (notice.hidden) announce(notice.textContent);
        notice.hidden = false;
      }
      held = true;
      return;
    }
    held = false;
    if (window.htmx) window.htmx.trigger(queue, NEWS);
  }

  // The badge's own span is the swap target; its answer is the element inside it.
  document.addEventListener("htmx:afterSwap", (e) => {
    const target = e.detail && e.detail.target;
    if (!target || !target.classList || !target.classList.contains("queue-badge")) return;
    const badge = target.querySelector("[data-rev]");
    if (!badge) return;
    const rev = badge.getAttribute("data-rev");
    // ⚠ The FIRST poll only establishes the baseline. Treating it as news would refresh the
    // list once on every page load, for nothing.
    if (seenRev === null) {
      seenRev = rev;
      return;
    }
    if (rev === seenRev) return;
    seenRev = rev;
    deliver();
  });

  // A held refresh, let through the moment the decision is over. `change` and `focusout`
  // are when a selection is put back or a field is emptied; the swap after a submitted
  // decision brings the fresh list with it, so nothing is held across it.
  // ⚠ On a tick, not inline: during `focusout` the focus has LEFT and not yet ARRIVED, so
  // `document.activeElement` is the body — a human tabbing from the severity select to the
  // publish button would read as idle for exactly that instant, and the list would swap out
  // from under the button they were reaching for.
  const retry = () => {
    if (!held) return;
    window.setTimeout(() => {
      if (!held) return;
      const queue = queueSection();
      if (queue && !midDecision(queue)) deliver();
    }, 0);
  };
  document.addEventListener("focusout", retry);
  document.addEventListener("change", retry);

  // ------------------------------------------- a ticked finding needs a word (ledger #657)
  //
  // ★ A batch with one ticked finding and no word for it is refused whole — rightly: a
  // refusal must never leave a batch half-applied — so the page stops that submit before
  // the round trip. Whether a finding is missing its word depends on two OTHER controls (its
  // own box, and the batch-wide word standing behind it), and no HTML attribute can say
  // that: `required` is static, and htmx has no conditional form of it. So this keeps each
  // finding's picker `required` exactly while its box is ticked and no batch word is chosen,
  // with the server's own sentence (`data-missing`) as the message. The browser's constraint
  // check then refuses the submit, names the finding and focuses its picker — it runs because
  // the FORM carries the htmx post. With scripting off none of this runs, and the server's
  // refusal does the same job: the form comes back as it was sent, every such finding marked.
  //
  // ⚠ It never blocks a finding the batch word covers, and an unticked one is never required.
  function checkWords(form) {
    const batchWord = form.querySelector('select[name="reason"]');
    const fallback = !!(batchWord && batchWord.value);
    const boxes = form.querySelectorAll('input[name="member"]');
    for (const picker of form.querySelectorAll("select[data-member]")) {
      const id = picker.getAttribute("data-member");
      const box = Array.prototype.find.call(boxes, (b) => b.value === id);
      const needs = !!(box && box.checked && !box.disabled) && !fallback;
      picker.required = needs;
      picker.setCustomValidity(
        needs && !picker.value ? picker.getAttribute("data-missing") || "" : ""
      );
    }
  }

  function checkAllWords() {
    for (const form of document.querySelectorAll("form.batch")) checkWords(form);
  }

  document.addEventListener("change", (e) => {
    const form = e.target && e.target.closest && e.target.closest("form.batch");
    if (form) checkWords(form);
  });
  // Every swap that can bring a batch form in: the list's own refresh, a decision, a refusal.
  document.addEventListener("htmx:afterSettle", checkAllWords);

  // The Layers page's post form is replaced by its answer (a fresh form, ledger #1675), and the
  // element that had focus goes with it. Focus goes back to the new form's text box, so a
  // keyboard user keeps their place; the "Posted" sentence is announced from #status.
  document.addEventListener("htmx:afterSettle", (e) => {
    const elt = e.detail && e.detail.elt;
    if (!elt || elt.id !== "say") return;
    const box = $("say-body");
    if (box) box.focus();
  });

  // --------------------------------- the Layers page's poll survives a failure (ledger #1705)
  //
  // The page's watcher asks once, a few seconds after it lands, and the answer carries the next
  // one. htmx swaps nothing on a failed ask, so one network error, or gonk restarting, used to
  // end the chain for good and the page silently stopped updating. Here a failed ask puts a new
  // watcher in its place that asks again after a backoff (5 s, doubling, at most 60 s), and the
  // page says so ONCE, in words, in its `#layers-reconnect` status (a polite live region, so
  // nothing moves focus and a retry is not re-announced). The first answer after that says
  // "Reconnected", and the next one empties the status.
  // ⚠ A 403 or a 404 is not retried: no retry can fix it. A session survives a gonk restart
  // (ledger #3229), so a 403 means it ended (signed out, revoked, or past its lifetime) and
  // every ask is refused; the status says to sign in again and reload, and nothing asks again.
  const WATCH_RETRY_FIRST = 5;
  const WATCH_RETRY_MAX = 60;
  const WATCH_PATH = "/layers/~/wait";

  function reconnectSay(text, state) {
    const status = $("layers-reconnect");
    if (!status) return;
    status.setAttribute("data-state", state);
    if (status.textContent === text) return;
    status.textContent = text;
    announce(text);
  }

  function watchFailed(elt, status) {
    if (status === 403) {
      reconnectSay(
        "Live updates stopped: gonk no longer accepts this page's sign-in (the session " +
          "ended or was signed out). Sign in again, then reload the page.",
        "stopped"
      );
      return;
    }
    if (status === 404) {
      reconnectSay("Live updates stopped: gonk no longer serves them here. Reload the page.", "stopped");
      return;
    }
    const failures = Number(elt.getAttribute("data-failures") || "0") + 1;
    const delay = Math.min(WATCH_RETRY_MAX, WATCH_RETRY_FIRST * Math.pow(2, failures - 1));
    reconnectSay(
      "Reconnecting: gonk did not answer, so new entries are not arriving. This page keeps " +
        "trying, less often as time goes on.",
      "retrying"
    );
    const next = document.createElement("div");
    next.className = "layers-watch";
    next.setAttribute("data-ui-quiet", "");
    next.hidden = true;
    next.setAttribute("hx-get", elt.getAttribute("hx-get"));
    next.setAttribute("hx-swap", "outerHTML");
    next.setAttribute("hx-trigger", "load delay:" + delay + "s");
    next.setAttribute("data-failures", String(failures));
    // On a tick: htmx is still finishing the failed request on the old element.
    window.setTimeout(() => {
      if (!elt.isConnected) return;
      elt.replaceWith(next);
      if (window.htmx) window.htmx.process(next);
    }, 0);
  }

  document.addEventListener("htmx:sendError", (e) => {
    const elt = e.detail && e.detail.elt;
    if (isWatch(elt)) watchFailed(elt.closest(".layers-watch"), 0);
  });
  document.addEventListener("htmx:responseError", (e) => {
    const elt = e.detail && e.detail.elt;
    const xhr = e.detail && e.detail.xhr;
    if (isWatch(elt)) watchFailed(elt.closest(".layers-watch"), xhr ? xhr.status : 0);
  });
  // ⚠ Matched by the request's PATH, not by `detail.elt`: a successful answer swaps the watcher
  // out (outerHTML) before htmx fires this, so htmx fires it on the nearest ancestor still in
  // the page and names THAT as `elt` (seen in a browser check, where an `isWatch(elt)` test
  // here never matched and the status said "Reconnecting" forever).
  document.addEventListener("htmx:afterRequest", (e) => {
    const detail = e.detail || {};
    const path = String((detail.pathInfo && detail.pathInfo.requestPath) || "");
    if (path.indexOf(WATCH_PATH) !== 0 || !detail.successful) return;
    const status = $("layers-reconnect");
    const state = status && status.getAttribute("data-state");
    if (state === "retrying") reconnectSay("Reconnected: live updates are back.", "back");
    else if (state === "back") reconnectSay("", "");
  });

  // ------------------------------------ a selected line fills the annotate quote (ledger #658)
  //
  // Brian, 2026-10-01: "Right now there is a way to select a line from the line number. I
  // think a little hook attached to that would be suitable." ikigai-browse's file view gives
  // every line `id="L{n}"` and a gutter self-link `href="#L{n}"`, so clicking a number selects
  // the line (`#L42` in the URL), and its annotate form asks for a quote typed by hand. browse
  // ships no scripts, so the hook lives here: when the selected line changes, the form's
  // `exact` gets that line's text — and `prefix`/`suffix` from the neighboring text, which
  // browse 0.15.0's form carries as hidden fields, so a quote that occurs twice anchors on the
  // line that was selected (a form without them still gets its quote). No new selection
  // mechanism: the gutter link and the hash are browse's, and with scripting off they work
  // exactly as before.
  //
  // ⚠ It never overwrites a quote the person TYPED: a fill is remembered on the field, and a
  // value that is neither empty nor the last fill is theirs.
  const LINE_HASH = /^#L(\d+)$/;
  const QUOTE_EDGE = 32;

  // The line's own text: the span less its gutter number and its markers, less its newline.
  function lineText(span) {
    const copy = span.cloneNode(true);
    for (const a of copy.querySelectorAll(
      "a.browse-ln, a.browse-annotation-marker, a.browse-proposal-marker"
    )) {
      a.remove();
    }
    return copy.textContent.replace(/\r?\n$/, "");
  }

  // Set a field the hook owns, unless the person has typed into it since the last fill.
  function fillField(field, value) {
    if (!field) return false;
    if (field.value !== "" && field.value !== field.dataset.gonkFilled) return false;
    field.value = value;
    field.dataset.gonkFilled = value;
    return true;
  }

  function fillQuote(n) {
    const span = document.getElementById("L" + n);
    if (!span || !span.matches(".browse-line") || !span.closest("#browse")) return;
    // The form of the view this line is in: the nearest ancestor that holds one.
    let scope = span.parentElement;
    while (scope && !scope.querySelector("form.browse-annotate")) scope = scope.parentElement;
    const form = scope && scope.querySelector("form.browse-annotate");
    if (!form) return;
    const raw = lineText(span);
    const quote = raw.trim();
    if (!quote) return; // a blank line anchors nothing
    if (!fillField(form.querySelector('input[name="exact"]'), quote)) return;
    const at = raw.indexOf(quote);
    const before = document.getElementById("L" + (Number(n) - 1));
    const after = document.getElementById("L" + (Number(n) + 1));
    const prefix = (before ? lineText(before) + "\n" : "") + raw.slice(0, at);
    const suffix = raw.slice(at + quote.length) + (after ? "\n" + lineText(after) : "");
    fillField(form.querySelector('input[name="prefix"]'), prefix.slice(-QUOTE_EDGE));
    fillField(form.querySelector('input[name="suffix"]'), suffix.slice(0, QUOTE_EDGE));
    flash("The annotate quote is now line " + n + ".", "ok");
  }

  function fillFromHash() {
    const m = location.hash.match(LINE_HASH);
    if (m) {
      showLine(m[1]);
      fillQuote(m[1]);
    }
  }

  // ★ The keyboard's way to a line (ledger #2905). Since ikigai-browse 0.20.6 a file is one tab
  // stop, the `<pre>`, and a gutter link is out of the tab order, so Tab no longer reaches the
  // hook above. The annotate form gets a line field and a button, which select the line by the
  // same hash and fill the same quote. The field shows the selected line, so the button alone
  // quotes a deep link's line. It has no `name`, so it is never posted, and no `min` or `max`:
  // a number left in it must not stop the annotation it has nothing to do with.
  function addLineControls() {
    if (!document.querySelector("#browse .browse-line")) return; // a pull request's form has none
    for (const form of document.querySelectorAll("#browse form.browse-annotate")) {
      if (form.querySelector("input.gonk-quote-line")) continue;
      const label = document.createElement("label");
      label.className = "browse-field";
      label.textContent = "line to quote ";
      const input = document.createElement("input");
      input.className = "gonk-quote-line";
      input.inputMode = "numeric";
      input.autocomplete = "off";
      label.appendChild(input);
      const button = document.createElement("button");
      button.type = "button";
      button.className = "gonk-quote-go";
      button.textContent = "quote line";
      const quote = form.querySelector('input[name="exact"]');
      form.insertBefore(button, quote.closest("label"));
      form.insertBefore(label, button);
      const m = location.hash.match(LINE_HASH);
      if (m) input.value = m[1];
    }
  }

  function showLine(n) {
    for (const input of document.querySelectorAll("#browse input.gonk-quote-line")) {
      input.value = n;
    }
  }

  // ⚠ A fragment navigation takes focus off the control, so it is put back: the person pressed
  // Enter there and has the note still to write. A typed quote stays, as above, and is said here
  // because this is a request, where a gutter click's fill is only a convenience.
  function quoteLine(form) {
    const wanted = form.querySelector("input.gonk-quote-line").value.trim();
    if (!/^[1-9]\d*$/.test(wanted)) {
      flash("The line to quote needs a line number.", "error");
      return;
    }
    const n = String(Number(wanted));
    const span = document.getElementById("L" + n);
    if (!span || !span.matches(".browse-line")) {
      const count = document.querySelectorAll("#browse .browse-line").length;
      flash("This file has " + count + " lines, so there is no line " + n + ".", "error");
      return;
    }
    if (!lineText(span).trim()) {
      flash("Line " + n + " is blank, so there is nothing to quote.", "error");
      return;
    }
    const quote = form.querySelector('input[name="exact"]');
    if (quote.value !== "" && quote.value !== quote.dataset.gonkFilled) {
      flash("The quote is one you typed, so it stays. Clear it to quote line " + n + ".", "error");
      return;
    }
    if (location.hash === "#L" + n) {
      fillQuote(n);
      return;
    }
    const focused = document.activeElement;
    location.hash = "#L" + n; // `hashchange` fills it, once
    focused.focus();
  }

  document.addEventListener("click", (e) => {
    const go = e.target && e.target.closest && e.target.closest("#browse button.gonk-quote-go");
    if (go) quoteLine(go.form);
  });
  // Enter in a form's field submits it; in this field it means the button beside it.
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Enter" || !e.target.matches || !e.target.matches("#browse input.gonk-quote-line")) {
      return;
    }
    e.preventDefault();
    quoteLine(e.target.form);
  });

  // The gutter click covers a line clicked twice, which moves no hash; `hashchange` covers
  // every other way the selection moves (back, forward, a typed URL).
  document.addEventListener("click", (e) => {
    const ln = e.target && e.target.closest && e.target.closest("#browse a.browse-ln");
    const m = ln && (ln.getAttribute("href") || "").match(LINE_HASH);
    if (m) {
      showLine(m[1]);
      fillQuote(m[1]);
    }
  });
  window.addEventListener("hashchange", fillFromHash);
  // A deep link into a view already on the page when this script runs (a deferred script
  // runs after parsing, so the lines are there if the server drew them inline).
  addLineControls();
  fillFromHash();
  // ★ A deep link SCROLLS to its line too (ledger #682 item 1). The browser scrolls to a
  // fragment once, at load, and the file view is not there yet: the browse shell fetches it
  // by htmx afterwards, so `#L17` stayed at the top of the page (measured: the line at
  // top=469, scrollY 0). So the swap that brings the line in scrolls to it, ONCE per load:
  // an annotate re-renders the same view with the same hash, and focuses its new card, and a
  // second scroll would pull the page away from it. A hash the person changes later is the
  // browser's own to scroll, and it does.
  let scrollPending = LINE_HASH.test(location.hash);

  // A deep link (`…#L42`) names a line before the file view has arrived: fill once it has.
  // ⚠ Only for the swap that brought that line in — the header badge settles a swap every
  // ten seconds on every page, and must not re-fill a field the person has just emptied.
  document.addEventListener("htmx:afterSettle", addLineControls);
  document.addEventListener("htmx:afterSettle", (e) => {
    const m = location.hash.match(LINE_HASH);
    const target = e.detail && e.detail.target;
    const line = m && target && target.querySelector && target.querySelector("#L" + m[1]);
    if (line) {
      if (scrollPending) {
        scrollPending = false;
        line.scrollIntoView({ block: "start" });
      }
      fillQuote(m[1]);
    }
  });

  // ---------------------------------- "On GitHub" follows the region (ledger #1907)
  //
  // The browse shell draws the link for the view it opens on (src/k.rs), and the region's own
  // affordances then move it to other views by htmx, which never changes this page's URL. So
  // when a swap into #browse came from `/k?c=source urn:repo:{root}:tree…`, `…:file:…` or a
  // pull request's `…:pr:{n}[:explain|:review]` (ledger #2117), this asks the server for the
  // same view's link (`urn:iki:gonk:github:…`, src/github.rs `link_iri`, which this map
  // mirrors) and swaps it in; a swap to any other view hides it. ⚠ The server builds and
  // encodes every URL: this relays which view is showing, and appends a file view's selected
  // line (`#L10`, `#L10-L20`, or the line an `#annotation-{id}` card names) to the server's
  // own `data-base`, after checking it is exactly that shape.
  const LINES_HASH = /^#L\d+(?:-L\d+)?$/;
  const GITHUB_VIEW = /^urn:repo:([^:]+):(?:(tree|tree:.+|file:.+)|pr:([1-9]\d*)(?::explain|:review)?)$/;

  // The GitHub name for a view: the same suffix for a tree or a file, `pull:{n}` for any of a
  // pull request's pages. Null for any other view.
  function githubOf(view) {
    const m = view.match(GITHUB_VIEW);
    if (!m) return null;
    return "urn:iki:gonk:github:" + m[1] + ":" + (m[3] ? "pull:" + m[3] : m[2]);
  }

  // The view a `/k?c=source {iri} …` read named, or null for anything else.
  function viewOf(path) {
    if (typeof path !== "string" || path.slice(0, 3) !== "/k?") return null;
    const command = new URLSearchParams(path.slice(3)).get("c") || "";
    const words = command.trim().split(/\s+/);
    return words[0] === "source" && words[1] ? words[1] : null;
  }

  function lineFragment() {
    const hash = location.hash;
    if (LINES_HASH.test(hash)) return hash;
    const m = hash.match(/^#annotation-(.+)$/);
    const card = m && document.getElementById("annotation-" + m[1]);
    const line = card && card.closest("#browse") && card.querySelector("a.browse-annotation-line");
    const href = line ? line.getAttribute("href") || "" : "";
    return LINES_HASH.test(href) ? href : "";
  }

  function carryLines() {
    const a = document.querySelector("#github a.github-link[data-lines]");
    if (a) a.setAttribute("href", (a.getAttribute("data-base") || "") + lineFragment());
  }

  function hideGithub() {
    const p = $("github");
    if (!p) return;
    p.hidden = true;
    p.textContent = "";
  }

  // The items about the file the page opened on (ledger #3667, src/file_items.rs) belong to
  // that file: when the region moves to another view, the section is hidden rather than left
  // beside a different file, and shown again when the region comes back to its file.
  document.addEventListener("htmx:afterSwap", (e) => {
    const target = e.detail && e.detail.target;
    const items = $("file-items");
    if (!target || target.id !== "browse" || !items) return;
    const config = e.detail.requestConfig;
    const view = viewOf(config && config.path);
    if (view) items.hidden = view !== items.getAttribute("data-browse-view");
  });

  document.addEventListener("htmx:afterSwap", (e) => {
    const target = e.detail && e.detail.target;
    if (!target || target.id !== "browse" || !$("github")) return;
    const config = e.detail.requestConfig;
    const view = viewOf(config && config.path);
    if (!view) return;
    const link = githubOf(view);
    if (!link) {
      hideGithub();
      return;
    }
    htmx.ajax("GET", "/k?c=" + encodeURIComponent("source " + link + " as=text/html"), {
      target: "#github",
      swap: "outerHTML",
    });
  });
  document.addEventListener("htmx:responseError", (e) => {
    if (isGithub(e.detail && e.detail.elt)) hideGithub();
  });
  document.addEventListener("htmx:sendError", (e) => {
    if (isGithub(e.detail && e.detail.elt)) hideGithub();
  });
  // A new link, a view that brought an annotation card in, and a moved selection.
  document.addEventListener("htmx:afterSettle", carryLines);
  window.addEventListener("hashchange", carryLines);
  carryLines();

  // ------------------------------------------------------- the browse region's history
  //
  // ★ BACK RETURNS TO THE VIEW YOU CAME FROM (Brian, 2026-10-24). Browse swaps every view into
  // #browse in place, so with nothing here the page's URL never moved and Back left browse
  // altogether. Each view the region shows now pushes its own page URL, `/browse/{iri}` with the
  // view's arguments as the query (the shell forwards them: `view_args` in src/k.rs), and Back or
  // Forward loads that view back into the region. So a PR opened from a tree's recent list goes
  // Back to the tree, the same place the repository crumb goes, and a reload or a shared URL
  // opens the same view, arguments and all.
  //
  // A restored view's URL already IS the location, so its swap pushes nothing; a fragment-only
  // move (`#L42`) fires popstate too, and is left to the line handling above.

  // The page URL for a `/k?c=source {iri} [k=v …]` read, or null for anything else (a Sink, a
  // positional value). `as` and `chrome` are the face, not the view, and are not carried.
  function pageUrlOf(path) {
    if (typeof path !== "string" || path.slice(0, 3) !== "/k?") return null;
    const command = new URLSearchParams(path.slice(3)).get("c") || "";
    const words = command.trim().split(/\s+/);
    if (words[0] !== "source" || !words[1]) return null;
    const query = new URLSearchParams();
    for (const word of words.slice(2)) {
      const at = word.indexOf("=");
      if (at < 1) return null;
      const name = word.slice(0, at);
      if (name === "as" || name === "chrome") continue;
      query.append(name, word.slice(at + 1));
    }
    const q = query.toString();
    return "/browse/" + encodeURI(words[1]) + (q ? "?" + q : "");
  }

  function here() {
    return location.pathname + location.search;
  }

  document.addEventListener("htmx:afterSwap", (e) => {
    const target = e.detail && e.detail.target;
    if (!target || target.id !== "browse") return;
    const config = e.detail.requestConfig;
    const url = pageUrlOf(config && config.path);
    if (!url || url === here()) return;
    history.pushState({ browse: url }, "", url);
  });

  window.addEventListener("popstate", () => {
    if (!$("browse") || location.pathname.slice(0, 8) !== "/browse/") return;
    const shown = $("browse").getAttribute("data-view-url");
    if (shown === here()) return;
    const iri = decodeURI(location.pathname.slice(8));
    let args = "";
    new URLSearchParams(location.search).forEach((value, name) => {
      args += " " + name + "=" + value;
    });
    htmx.ajax("GET", "/k?c=" + encodeURIComponent("source " + iri + args + " as=text/html"), {
      target: "#browse",
      swap: "innerHTML",
    });
  });

  // Which page URL the region is showing, so a fragment-only Back reloads nothing.
  document.addEventListener("htmx:afterSettle", (e) => {
    const target = e.detail && e.detail.target;
    if (!target || target.id !== "browse") return;
    const url = pageUrlOf(e.detail.requestConfig && e.detail.requestConfig.path);
    if (url) target.setAttribute("data-view-url", url);
  });

  // --------------------------------------------- the Dashboard's polling (ledger #2967)
  //
  // Every chart and agent table is a fragment with `hx-trigger="every 30s, gonk:refresh"`
  // (src/dashboard.rs); this is what htmx cannot say. WCAG 2.2.2: a person can pause it, and the
  // choice is remembered in this browser. An ask is not sent while paused, while the tab is hidden
  // (Page Visibility), or for a panel focus is inside: replacing it would take the focused link,
  // and the shell would then have to move focus, so that panel waits for the next ask after focus
  // leaves. Coming back asks at once, so nobody reads old numbers for up to 30 s.
  //
  // ★ Held at `htmx:confirm`, the one event before the shell marks the target `aria-busy`: a request
  // canceled after that mark never reaches the `afterRequest` that clears it.
  //
  // ★ `If-None-Match` is sent from here because the answers are `no-store`: no browser cache holds
  // a viewer's numbers, so no browser would send it. htmx's rule for `[23]..` is to swap, and a 304
  // has no body, so a 304 must be told not to.
  //
  // ★ An answer that differs only in its links is not news. A table with agents has links that
  // name the minute, so its bytes change once a minute whatever the numbers do. It is compared
  // without them and left alone: a link keeps asking what its number answered, and a phone reader's
  // sideways scroll stays. What the person opened stays open, and a real update is one polite
  // sentence however many fragments moved.
  const LIVE_PAUSED = "gonk:dashboard-paused";
  const LIVE_SAY_AFTER_MS = 500;
  const liveTags = new Map();
  const liveScroll = new Map();
  let livePaused = false;
  let liveFailing = false;
  let liveStopped = false;
  let liveSaid = null;
  let liveSayTimer = null;

  const isLive = (elt) => !!(elt && elt.matches && elt.matches("[data-live]"));
  const livePolls = () => document.querySelectorAll("[data-live][hx-get]");

  function liveHeld(elt) {
    return livePaused || liveStopped || document.hidden || elt.contains(document.activeElement);
  }

  function liveRemembered() {
    try {
      return window.localStorage.getItem(LIVE_PAUSED) === "true";
    } catch (_) {
      return false;
    }
  }

  function liveRemember(paused) {
    try {
      if (paused) window.localStorage.setItem(LIVE_PAUSED, "true");
      else window.localStorage.removeItem(LIVE_PAUSED);
    } catch (_) {
      // Storage refused (a private window, blocked site data): paused until the page is left.
    }
  }

  // The state in words, on screen. Only a change the person did not just make is announced.
  function liveDraw() {
    const button = $("dashboard-pause");
    const state = $("dashboard-live-state");
    if (!button || !state) return;
    button.hidden = liveStopped;
    button.textContent = livePaused ? "Resume updates" : "Pause updates";
    state.textContent = liveStopped
      ? "Updates stopped: gonk no longer accepts this page's sign-in. Sign in again, then reload the page."
      : livePaused
        ? "Updates are paused. These numbers stay as they are until you resume."
        : liveFailing
          ? "gonk did not answer the last update, so these numbers may be behind. This page keeps asking."
          : state.getAttribute("data-running");
  }

  function liveRefresh() {
    if (livePaused || liveStopped || document.hidden || !window.htmx) return;
    for (const elt of livePolls()) window.htmx.trigger(elt, "gonk:refresh");
  }

  function liveFailed(status) {
    if (status === 403 || status === 404) {
      if (liveStopped) return;
      liveStopped = true;
      liveDraw();
      announce($("dashboard-live-state").textContent);
    } else if (!liveFailing) {
      liveFailing = true;
      liveDraw();
      announce($("dashboard-live-state").textContent);
    }
  }

  function liveAnswered() {
    if (!liveFailing) return;
    liveFailing = false;
    liveDraw();
  }

  // One sentence for a round of polls, replacing the last so a tab left open does not grow the
  // region. A new node is what a screen reader hears, so the same words are said again.
  function liveSay() {
    clearTimeout(liveSayTimer);
    liveSayTimer = setTimeout(() => {
      if (liveSaid) liveSaid.remove();
      liveSaid = announce("Counts updated.");
    }, LIVE_SAY_AFTER_MS);
  }

  function liveSayAttention(elt, next) {
    const bad = (root) => root.querySelector(".needs-attention");
    const was = bad(elt);
    const now = bad(next);
    if (!was === !now) return;
    const heading = next.querySelector("h3");
    const name = heading ? heading.textContent.trim() : "A row";
    announce(now ? name + " needs attention." : name + " no longer needs attention.");
  }

  // What a fragment shows, whichever of the live page or an answer it is read from.
  function liveShape(elt) {
    const copy = elt.cloneNode(true);
    for (const a of copy.querySelectorAll('a[target="_blank"]')) a.removeAttribute("href");
    for (const d of copy.querySelectorAll("details")) d.removeAttribute("open");
    return copy.innerHTML;
  }

  document.addEventListener("htmx:confirm", (e) => {
    if (isLive(e.target) && liveHeld(e.target)) e.preventDefault();
  });
  document.addEventListener("htmx:configRequest", (e) => {
    if (!isLive(e.target)) return;
    const tag = liveTags.get(e.target.getAttribute("hx-get"));
    if (tag) e.detail.headers["If-None-Match"] = tag;
  });
  document.addEventListener("htmx:beforeSwap", (e) => {
    const elt = e.target;
    if (!isLive(elt)) return;
    const detail = e.detail;
    if (detail.xhr.status === 304) {
      detail.shouldSwap = false;
      liveAnswered();
      return;
    }
    if (!detail.shouldSwap) return; // a refusal or a failure, said below
    liveAnswered();
    // Held since it was asked for: no validator is kept, so the next ask brings this answer again.
    if (liveHeld(elt)) {
      detail.shouldSwap = false;
      return;
    }
    const key = elt.getAttribute("hx-get");
    const tag = detail.xhr.getResponseHeader("ETag");
    if (tag) liveTags.set(key, tag);
    const holder = document.createElement("template");
    holder.innerHTML = detail.serverResponse;
    const next = holder.content.firstElementChild;
    if (!next || liveShape(next) === liveShape(elt)) {
      detail.shouldSwap = false;
      return;
    }
    const open = Array.from(elt.querySelectorAll("details"), (d) => d.open);
    next.querySelectorAll("details").forEach((d, n) => {
      if (open[n]) d.setAttribute("open", "");
    });
    detail.serverResponse = holder.innerHTML;
    const frame = elt.querySelector(".table-wrap");
    if (frame && frame.scrollLeft) liveScroll.set(key, frame.scrollLeft);
    const kind = elt.getAttribute("data-live");
    // An operator's readout moves on every poll (the uptime, the cache), which is not news. What
    // needs attention changing is, in words (ledger #2972).
    if (kind === "readout") liveSayAttention(elt, next);
    else if (kind !== "note") liveSay();
  });
  document.addEventListener("htmx:afterSettle", () => {
    if (!liveScroll.size) return;
    for (const elt of livePolls()) {
      const left = liveScroll.get(elt.getAttribute("hx-get"));
      const frame = left && elt.querySelector(".table-wrap");
      if (frame) frame.scrollLeft = left;
    }
    liveScroll.clear();
  });
  document.addEventListener("htmx:responseError", (e) => {
    if (isLive(e.target)) liveFailed(e.detail.xhr ? e.detail.xhr.status : 0);
  });
  document.addEventListener("htmx:sendError", (e) => {
    if (isLive(e.target)) liveFailed(0);
  });

  function wireDashboard() {
    const button = $("dashboard-pause");
    if (!button) return;
    wireDashboardTabs();
    livePaused = liveRemembered();
    button.hidden = false;
    liveDraw();
    button.addEventListener("click", () => {
      livePaused = !livePaused;
      liveRemember(livePaused);
      liveDraw();
      announce(livePaused ? "Updates paused." : "Updates resumed.");
      liveRefresh();
    });
    document.addEventListener("visibilitychange", liveRefresh);
  }

  // --------------------------------------------- the Dashboard's tabs (ledger #2971)
  //
  // Each tab is a link to a whole page, which is all it is with no script. Here it is the WAI-ARIA
  // tabs pattern. A tab shows as it is selected, arrow keys included: its panel is one fetch of
  // bytes the server keeps cached. Focus stays on the tab, never going into the panel by itself.
  //
  // ★ THE HIDDEN PANEL IS EMPTY, not held. Its fragments, and with them their timers, are gone, so
  // it cannot poll, and coming back fetches it fresh once whether or not the updates are paused: a
  // person asked to see it, which is not the updating that WCAG 2.2.2 lets them stop. From then on
  // its fragments poll, and `liveHeld` applies, as for a panel the page loaded with.
  //
  // ★ `fetch`, not an htmx swap, so the shell's rule for a swap (focus to what it brought) has
  // nothing to act on.
  let tabAsk = null;

  const dashboardTabs = () => Array.from(document.querySelectorAll("#dashboard-tabs a"));
  const tabPanel = (tab) => $(tab.getAttribute("data-panel"));
  const tabShown = () =>
    dashboardTabs().find((tab) => tab.getAttribute("aria-selected") === "true");

  // What this URL shows: `/dashboard` is the first tab.
  function tabOfLocation() {
    const tabs = dashboardTabs();
    return tabs.find((tab) => tab.getAttribute("href") === location.pathname) || tabs[0];
  }

  function tabSelect(shown) {
    for (const tab of dashboardTabs()) {
      const selected = tab === shown;
      tab.setAttribute("aria-selected", selected ? "true" : "false");
      tab.tabIndex = selected ? 0 : -1;
      tabPanel(tab).hidden = !selected;
    }
    document.title = shown.getAttribute("data-title");
  }

  // Said in the panel, so it stays until the person acts, with the way out that needs no script.
  function tabFailed(tab, panel) {
    const note = document.createElement("p");
    note.className = "note";
    note.textContent = "gonk did not answer, so this panel did not load. ";
    const again = document.createElement("a");
    again.href = tab.getAttribute("href");
    again.textContent = "Load it again";
    note.appendChild(again);
    panel.replaceChildren(note);
    announce("gonk did not answer, so this panel did not load.");
  }

  async function tabShow(tab) {
    const from = tabShown();
    if (tab === from) return;
    if (tabAsk) tabAsk.abort();
    const ask = new AbortController();
    tabAsk = ask;
    const left = tabPanel(from);
    for (const elt of left.querySelectorAll("[data-live][hx-get]")) {
      window.htmx.trigger(elt, "htmx:abort");
    }
    // Back from a link in the panel: its element is going, and the tab is the nearest place.
    if (left.contains(document.activeElement)) tab.focus();
    left.replaceChildren();
    tabSelect(tab);
    const panel = tabPanel(tab);
    const waiting = document.createElement("p");
    waiting.className = "note";
    waiting.textContent = "Loading…";
    panel.replaceChildren(waiting);
    let response;
    let html;
    try {
      response = await fetch(tab.getAttribute("data-panel-href"), {
        headers: { Accept: "text/html" },
        credentials: "same-origin",
        signal: ask.signal,
      });
      if (response.ok) html = await response.text();
    } catch (_) {
      if (ask.signal.aborted) return; // a later switch took over
    }
    if (ask.signal.aborted) return;
    if (html === undefined) {
      tabFailed(tab, panel);
      if (response) liveFailed(response.status);
      return;
    }
    panel.innerHTML = html;
    window.htmx.process(panel);
    liveAnswered();
  }

  function tabPick(tab) {
    if (tab === tabShown()) return;
    history.pushState({ dashboardTab: tab.getAttribute("href") }, "", tab.getAttribute("href"));
    tabShow(tab);
  }

  function tabKey(e) {
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    const tabs = dashboardTabs();
    const at = tabs.indexOf(e.target);
    if (at < 0) return;
    let next;
    if (e.key === "ArrowRight") next = tabs[(at + 1) % tabs.length];
    else if (e.key === "ArrowLeft") next = tabs[(at - 1 + tabs.length) % tabs.length];
    else if (e.key === "Home") next = tabs[0];
    else if (e.key === "End") next = tabs[tabs.length - 1];
    else if (e.key === " ") next = tabs[at]; // a tab answers Space, and a link would scroll
    else return;
    e.preventDefault();
    next.focus();
    tabPick(next);
  }

  // With no htmx nothing here could poll or swap, so the links stay the plain links they work as.
  function wireDashboardTabs() {
    const list = $("dashboard-tabs");
    if (!list || !window.htmx) return;
    list.setAttribute("role", "tablist");
    list.setAttribute("aria-label", "Dashboard views");
    const shown = dashboardTabs().find((tab) => tab.hasAttribute("aria-current"));
    for (const tab of dashboardTabs()) {
      tab.removeAttribute("aria-current");
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-controls", tab.getAttribute("data-panel"));
      tabPanel(tab).setAttribute("role", "tabpanel");
      tab.addEventListener("click", (e) => {
        // A new tab or window is the link's own business.
        if (e.button || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
        e.preventDefault();
        tabPick(tab);
      });
    }
    tabSelect(shown);
    list.addEventListener("keydown", tabKey);
    window.addEventListener("popstate", () => {
      const tab = tabOfLocation();
      if (tab === tabShown()) return;
      tabShow(tab);
      // No key or click brought this, so nothing else has said which tab is showing.
      announce(tab.textContent.trim() + " shown.");
    });
  }

  // ------------------------------------------------------------------ sign-in notices

  // ★ A PAGE THE GRANT DOES NOT REACH SAYS SO IN WORDS (ledger #739). The server renders both
  // sentences of src/web.rs `Lacking` — the signed-out one visible, the signed-in one and its
  // grant footnote hidden — because a page is a READ and `ikigai-web` hands the principal to
  // writes only. Whether there is a session is answered below by `/auth/session`, which is
  // `Passkeys::identity` over the same cookie the door's `http_principal` reads: the same
  // question, asked of the same function, once per page.
  //
  // `signedIn` stays false until that answer arrives, and the notice is re-drawn on every
  // settle because the Queue swaps its own section in, notice and all.
  let signedIn = false;
  let canSignIn = false;

  function drawNotices(root) {
    const scope = root && root.querySelectorAll ? root : document;
    // The header's withheld-sections sentence (ledger #446) switches the same way.
    scope.querySelectorAll("[data-signin-notice], [data-withheld-notice]").forEach((notice) => {
      const out = notice.querySelector(".signed-out");
      const inside = notice.querySelector(".signed-in");
      const button = notice.querySelector(".signin-here");
      if (out) out.hidden = signedIn;
      if (inside) inside.hidden = !signedIn;
      if (button) button.hidden = signedIn || !canSignIn;
    });
    describeSignIn(scope);
  }

  // ★ WITH NO PASSKEY ENROLLED, A SIGN-IN BUTTON SAYS WHY IT WILL FAIL (ledger #1847). The sentence
  // is the server's, in the page; this points each button at it, as a note beside its control is
  // (ACCESSIBILITY.md), keeping a description the button already has (a refusal's sentence).
  function describeSignIn(scope) {
    const note = $("first-passkey");
    if (!note || note.hidden) return;
    scope.querySelectorAll("#auth-login, .signin-here").forEach((button) => {
      const ids = (button.getAttribute("aria-describedby") || "").split(/\s+/).filter(Boolean);
      if (!ids.includes(note.id)) button.setAttribute("aria-describedby", ids.concat(note.id).join(" "));
    });
  }

  // An invite link in hand, or a passkey just created, makes the sentence false. A hidden element
  // still describes what points at it, so the buttons let go of it too.
  function hideFirstPasskey() {
    const note = $("first-passkey");
    if (!note) return;
    note.hidden = true;
    document.querySelectorAll('[aria-describedby~="first-passkey"]').forEach((button) => {
      const ids = button.getAttribute("aria-describedby").split(/\s+/).filter((id) => id !== note.id);
      if (ids.length) button.setAttribute("aria-describedby", ids.join(" "));
      else button.removeAttribute("aria-describedby");
    });
  }

  // ★ A REFUSAL PAGE PUTS FOCUS WHERE THE WAY ON IS (ledger #2021, D13). The page a browser gets
  // for an error it navigated to (src/refusal.rs) marks its heading `autofocus` for a page with
  // no script, but a browser may skip autofocus (Chromium did, on a navigation in a pane without
  // focus), so the script focuses it as it starts. Once the sign-in button above is shown, it is
  // the one thing to do, so focus moves to it. Heading and button are each described by the
  // refusal's sentence, so a screen reader reads the sentence with whichever has focus.
  function focusRefusal() {
    const refusal = document.querySelector("[data-refusal]");
    if (!refusal) return;
    const button = refusal.querySelector(".signin-here");
    const heading = refusal.querySelector("h1");
    const target = button && !button.hidden ? button : heading;
    if (target && document.activeElement !== target) target.focus();
  }

  document.addEventListener("htmx:afterSettle", (e) => drawNotices(e.detail && e.detail.target));
  document.addEventListener("click", (e) => {
    if (canSignIn && e.target.closest && e.target.closest(".signin-here")) signIn();
  });

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
  // canceled sheet, a timeout, or a page without focus — and its DOM text ("The document is
  // not focused", "The operation either timed out or was not allowed") says nothing a person
  // can act on. Say what to do instead. Anything else (a server refusal) is already words.
  function ceremonyError(err, retry) {
    if (err && err.name === "NotAllowedError") {
      return "the passkey prompt was cancelled, timed out, or opened while this window did not have focus. " + retry;
    }
    return (err && err.message) || String(err);
  }

  // ★ WHAT THE SYSTEM PASSKEY PICKER SHOWS (ledger #3387). Every gonk is the one `localhost`
  // relying party, so the picker lists every gonk's passkeys together, by the WebAuthn
  // `user.name` and `user.displayName` the passkey was created with. The name is the label and
  // the host ("laptop @ localhost:1062"), the display name the label and the grant ("laptop ·
  // owner-viewer"): both are shown to anyone who opens the device's passkey manager, so neither
  // carries anything secret (no invite, no token, no credential id). An authenticator need keep
  // only 64 bytes of each, so a long label is shortened there, the host and the grant kept whole.
  const NAME_BYTES = 64;
  const utf8 = new TextEncoder();

  function fitBytes(text, max) {
    if (utf8.encode(text).length <= max) return text;
    const chars = Array.from(text);
    while (chars.length && utf8.encode(chars.join("") + "…").length > max) chars.pop();
    return chars.join("") + "…";
  }

  function passkeyNames(label, grant) {
    const named = (tail) => fitBytes(fitBytes(label, NAME_BYTES - utf8.encode(tail).length) + tail, NAME_BYTES);
    return { name: named(" @ " + location.host), displayName: named(" · " + grant) };
  }

  // The names are frozen at creation, and a passkey's grant (or label) can change after it.
  // Where the browser has the WebAuthn Level 3 signal, each sign-in tells the passkey manager
  // the current ones; where it has not, nothing is said and nothing fails.
  async function signalNames(cred, done) {
    const signal = window.PublicKeyCredential && PublicKeyCredential.signalCurrentUserDetails;
    const handle = cred.response && cred.response.userHandle;
    if (typeof signal !== "function" || !handle || !done.label || !done.grant) return;
    try {
      // Bounded: a signal that never settles must not hold the signed-in page's reload.
      await Promise.race([
        signal.call(PublicKeyCredential, {
          rpId: "localhost",
          userId: toB64(handle),
          ...passkeyNames(done.label, done.grant),
        }),
        new Promise((resolve) => setTimeout(resolve, 2000)),
      ]);
    } catch (_) {
      // Only the picker's words are at stake; the sign-in itself is done.
    }
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
      await signalNames(cred, done);
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
      const name = label || "gonk passkey";
      // The label goes first, so one another passkey already has is refused before the system
      // sheet mints a credential that could never be enrolled (ledger #3384); the invite, so
      // the answer names the grant the passkey's display name carries (#3387).
      const { challenge, grant } = await post("/auth/register-options", { label: name, invite });
      const user = new Uint8Array(16);
      crypto.getRandomValues(user);
      const cred = await navigator.credentials.create({
        publicKey: {
          challenge: fromB64(challenge),
          rp: { id: "localhost", name: "gonk" },
          user: { id: user, ...passkeyNames(name, grant) },
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
      hideFirstPasskey();
      // ★ Do NOT chain navigator.credentials.get() here. When create() resolves, the system
      // passkey sheet still holds focus, and Chrome refuses get() from an unfocused document
      // ("The document is not focused") — found by the first real Touch ID enrollment. A click
      // on the sign-in button is a fresh user gesture on a page that has focus back.
      const created = "Passkey created for " + done.label + " (grant " + done.grant + "). ";
      // ★ SIGNED IN, THE OLD SESSION ENDS HERE (ledger #3384). Kept, every later request ran as
      // the OLD passkey and its grant, which reads as a permission bug. Ended in place, with no
      // reload, so the sentence and the focus below survive: the same one click on Sign in as
      // the signed-out case.
      let ended = "";
      if (!$("auth-logout").hidden) {
        const t = token();
        setToken("", 0);
        try {
          if (t) await post("/auth/logout", t);
        } catch (_) {
          // Signed out locally either way; the server session expires on its own.
        }
        ended = $("auth-who").textContent.replace(/^Signed in as /, "") + " is signed out. ";
        $("auth-who").textContent = "";
        $("auth-who").hidden = true;
        $("auth-logout").hidden = true;
        signedIn = false;
        drawNotices(document);
      }
      const login = $("auth-login");
      login.hidden = false;
      flash(created + ended + "Now click Sign in with passkey to use it.", "ok");
      login.focus();
    } catch (err) {
      flash(
        "Could not create the passkey: " +
          ceremonyError(err, "The invite is still good until it expires — click Create passkey to try again."),
        "error"
      );
      // A refused label is changed in the field; a retry is its form's own button.
      const field = $("enrol-label");
      if (field && !$("enrol").hidden) field.focus();
    }
  }

  // ★ SIGNED IN, AN INVITE WIDENS THE PASSKEY SIGNED IN WITH (ledger #3388). A grant is shared by
  // every passkey that holds it, so one person's access is widened by adding the invite's grant
  // to their own passkey, never by editing the grant. The session cookie alone is not enough:
  // the passkey it signs in as must answer a fresh challenge (user verification required), so a
  // stolen cookie plus the link raises nothing. The server ends every session of the passkey and
  // answers a new one, which replaces the cookie; the page reloads to draw what the wider grant
  // opens, and the sentence is said after the reload.
  async function addGrant(invite) {
    try {
      const t = token();
      const { challenge, credential } = await post("/auth/add-grant-options", { session: t, invite });
      const cred = await navigator.credentials.get({
        publicKey: {
          challenge: fromB64(challenge),
          rpId: "localhost",
          allowCredentials: [{ type: "public-key", id: fromB64(credential) }],
          userVerification: "required",
          timeout: 120000,
        },
      });
      const r = cred.response;
      const done = await post("/auth/add-grant", {
        session: t,
        invite,
        challenge,
        id: cred.id,
        authenticatorData: toB64(r.authenticatorData),
        clientDataJSON: toB64(r.clientDataJSON),
        signature: toB64(r.signature),
      });
      setToken(done.session, done.seconds);
      await signalNames(cred, done);
      later("Grant " + done.added + " added to the passkey " + done.label + ", which now holds " + done.grant + ". You are signed in again with it.");
      history.replaceState(null, "", location.pathname + location.search);
      location.reload();
    } catch (err) {
      flash(
        "Could not add the grant: " +
          ceremonyError(err, "The invite is still good until it expires — click Add grant to try again."),
        "error"
      );
      const submit = $("enrol-submit");
      if (submit && !$("enrol").hidden) submit.focus();
    }
  }

  // A sentence for the page a reload is about to draw, said once by it (and only in this tab).
  const LATER = "gonk_flash";
  function later(text) {
    try {
      window.sessionStorage.setItem(LATER, text);
    } catch (_) {
      // Storage refused: the reloaded page is the evidence, without the sentence.
    }
  }
  function sayLater() {
    try {
      const text = window.sessionStorage.getItem(LATER);
      window.sessionStorage.removeItem(LATER);
      if (text) flash(text, "ok");
    } catch (_) {
      // Nothing was kept.
    }
  }

  // The invite panel's choice, signed in: add (the default) shows no Label field, create does.
  function wireEnrolChoice() {
    const add = $("enrol-add");
    const choice = $("enrol-choice");
    if (!add || !choice) return () => false;
    choice.hidden = false;
    const note = $("enrol-signed-in");
    if (note) note.hidden = false;
    const draw = () => {
      $("enrol-new").hidden = add.checked;
      $("enrol-submit").textContent = add.checked ? "Add grant to this passkey" : "Create passkey";
    };
    choice.addEventListener("change", draw);
    draw();
    return () => add.checked;
  }

  // ------------------------------------------------------------------ samples

  // A sample button puts its query in the editor and stops there. It never submits: the
  // person presses Run, so a half-typed query is never lost to a click, and a query that
  // would be expensive is never started by accident. The text lives in a hidden <pre> the
  // server rendered (newlines survive an element; an attribute's would not).
  //
  // ★ The buttons are mutually exclusive TOGGLES, and the state they carry is a claim about
  // the editor: `aria-pressed="true"` means "the box holds this sample's query". So the
  // interesting half is not setting it but CLEARING it — the moment a character is typed the
  // claim is false, and a button still making it is the UI lying about what is on screen.
  // Cleared on `input`, not on blur and not on submit.
  //
  // ⚠ Typing and then undoing back to the exact sample text leaves it cleared, deliberately.
  // Re-deriving the state by comparing the text would be a guess about intent, and it is
  // wrong in the other direction too: two samples can be edited into each other.
  function wireSamples() {
    const box = $("q");
    if (!box) return;
    const buttons = document.querySelectorAll("button.sample");
    const active = () => document.querySelector('button.sample[aria-pressed="true"]');
    const press = (button) => {
      for (const b of buttons) b.setAttribute("aria-pressed", b === button ? "true" : "false");
    };
    for (const button of buttons) {
      button.addEventListener("click", () => {
        const source = $(button.getAttribute("data-query"));
        if (!source) return;
        // Assigning `.value` fires no `input` event, so this does not immediately undo
        // itself. Re-clicking the pressed sample restores its text after an edit.
        box.value = source.textContent;
        press(button);
        box.focus();
        box.setSelectionRange(box.value.length, box.value.length);
        flash("Loaded the sample query. Press Run to execute it.", "ok");
      });
    }
    // Only when something is actually pressed: rewriting nine unchanged attributes on every
    // keystroke is work a screen reader may notice.
    box.addEventListener("input", () => {
      if (active()) press(null);
    });
  }

  // ★ THE HEADER'S LEDGER MENU (ledger #1888). The server draws a <details>, which works with
  // no script at all; this turns its <summary> into a real disclosure BUTTON, because a summary
  // is not one to every screen reader, and the button says its state (aria-expanded) and what
  // it opens (aria-controls). It stays a menu of LINKS: nothing navigates until a link is
  // followed (WCAG 3.2.2, the reason it is not a <select>).
  //
  // Escape closes it from anywhere inside and puts focus back on the button, which is where a
  // keyboard user opened it; a click outside closes it and moves no focus. Focus is not moved
  // on opening either: the links are next in tab order.
  function wireLedgerMenu() {
    const details = document.querySelector("details[data-ledger-menu]");
    const panel = $("ledger-panel");
    if (!details || !panel) return;
    const summary = details.querySelector("summary");
    const menu = document.createElement("div");
    menu.className = "ledger-menu";
    const button = document.createElement("button");
    button.type = "button";
    button.id = "ledger-toggle";
    button.className = "ledger-toggle";
    button.textContent = summary ? summary.textContent : "Ledgers";
    const mark = document.createElement("span");
    mark.setAttribute("aria-hidden", "true");
    mark.textContent = "\u25BE";
    button.appendChild(mark);
    button.setAttribute("aria-expanded", "false");
    button.setAttribute("aria-controls", panel.id);
    panel.hidden = true;
    details.replaceWith(menu);
    menu.append(button, panel);

    const isOpen = () => button.getAttribute("aria-expanded") === "true";
    const setOpen = (open) => {
      button.setAttribute("aria-expanded", open ? "true" : "false");
      panel.hidden = !open;
    };
    button.addEventListener("click", () => setOpen(!isOpen()));
    menu.addEventListener("keydown", (e) => {
      if (e.key !== "Escape" || !isOpen()) return;
      e.preventDefault();
      setOpen(false);
      button.focus();
    });
    document.addEventListener("click", (e) => {
      if (isOpen() && !menu.contains(e.target)) setOpen(false);
    });
    wireLedgerFilter(panel);
  }

  // The filter, drawn by the server only past eight ledgers and hidden until here, because a
  // box that does nothing without a script is worse than no box. It narrows the links by name
  // as a person types, and says how many match in its polite live region, a moment after the
  // typing stops so a screen reader is not handed one sentence per keystroke.
  function wireLedgerFilter(panel) {
    const box = panel.querySelector("[data-ledger-filter]");
    const input = $("ledger-filter");
    const status = $("ledger-filter-status");
    if (!box || !input || !status) return;
    const items = Array.from(panel.querySelectorAll("li[data-ledger-name]"));
    box.hidden = false;
    let pending = null;
    input.addEventListener("input", () => {
      const want = input.value.trim().toLowerCase();
      let shown = 0;
      for (const item of items) {
        const match = !want || item.getAttribute("data-ledger-name").includes(want);
        item.hidden = !match;
        if (match) shown++;
      }
      const total = items.length;
      const sentence = !want
        ? total + " ledgers."
        : shown === 0
          ? "No ledger matches."
          : shown + " of " + total + (shown === 1 ? " ledger matches." : " ledgers match.");
      clearTimeout(pending);
      pending = setTimeout(() => {
        status.textContent = sentence;
        announce(sentence);
      }, 300);
    });
  }

  // SPARQL results swap in place; what is said is their one-line summary, never the table.
  document.addEventListener("htmx:afterSettle", (e) => {
    const target = e.detail && e.detail.target;
    if (!target || target.id !== "results") return;
    const meta = target.querySelector(".result-meta");
    if (meta) announce(meta.textContent);
  });

  // Safe mode (ledger #3379): the footer's links keep the page's own query, and with a script
  // the per-browser toggle is offered. web/gonk-safe.js, linked ahead of a deployment's assets,
  // is what honors the toggle on the next page.
  const SAFE = "gonk:safe";
  // This browser's storage, or null where it is refused (a private window, a blocked origin).
  function storage() {
    try {
      return window.localStorage;
    } catch (_) {
      return null;
    }
  }
  function safeUrl(on) {
    const url = new URL(location.href);
    if (on) url.searchParams.set("safe", "1");
    else url.searchParams.delete("safe");
    return url.href;
  }
  function wireSafeMode() {
    const note = $("deployment-note");
    if (!note) return;
    const enter = note.querySelector("a.safe-enter");
    if (enter) enter.href = safeUrl(true);
    const leave = note.querySelector("a.safe-leave");
    if (leave) leave.href = safeUrl(false);
    const store = storage();
    const toggle = $("safe-toggle");
    if (!store || !toggle) return; // No storage: the query flag is the whole of safe mode here.
    // Leaving safe mode in a browser that asked for it on every page stops that too, or the
    // gate would bring the page straight back.
    if (leave) leave.addEventListener("click", () => store.removeItem(SAFE));
    let stored = store.getItem(SAFE) === "1";
    const draw = () => {
      toggle.textContent = stored
        ? "Stop using safe mode in this browser"
        : "Use safe mode on every page in this browser";
    };
    draw();
    toggle.hidden = false;
    toggle.addEventListener("click", () => {
      stored = !stored;
      if (stored) store.setItem(SAFE, "1");
      else store.removeItem(SAFE);
      draw();
      location.replace(safeUrl(stored));
    });
  }

  // An item's comments, oldest or newest first (ledger #3379): the links work with no script;
  // here they reorder the list in place, and the choice is remembered in this browser and kept
  // across the item's swaps (a comment posted re-renders it in written order).
  const COMMENTS = "gonk:comments";
  function commentOrderAsked() {
    const asked = new URL(location.href).searchParams.get("comments");
    if (asked === "newest" || asked === "oldest") return asked;
    const store = storage();
    return store && store.getItem(COMMENTS) === "newest" ? "newest" : "oldest";
  }
  function commentOrder(order, say) {
    const control = $("comment-order");
    const list = document.querySelector("#item .comment-list");
    if (!control || !list) return;
    if (list.getAttribute("data-order") !== order) {
      for (const comment of Array.from(list.children).reverse()) list.appendChild(comment);
      list.setAttribute("data-order", order);
    }
    for (const link of control.querySelectorAll("a[data-order]")) {
      if (link.getAttribute("data-order") === order) link.setAttribute("aria-current", "true");
      else link.removeAttribute("aria-current");
    }
    if (say) announce(order === "newest" ? "Comments, newest first." : "Comments, oldest first.");
  }
  function wireCommentOrder() {
    commentOrder(commentOrderAsked(), false);
  }
  document.addEventListener("click", (e) => {
    const link = e.target.closest && e.target.closest("#comment-order a[data-order]");
    if (!link) return;
    e.preventDefault();
    const order = link.getAttribute("data-order");
    const store = storage();
    if (store) store.setItem(COMMENTS, order); // With none, this page still reorders.
    const url = new URL(location.href);
    url.searchParams.set("comments", order);
    history.replaceState(history.state, "", url.href);
    commentOrder(order, true);
  });
  // A swap that re-renders the item (a comment posted, an edit) draws it in written order.
  document.addEventListener("htmx:afterSwap", wireCommentOrder);

  // ------------------------------------------------ the server moved (ledger #375)
  //
  // Every full page carries the instance id of the process that served it
  // (`<meta name="gonk-instance">`), and /instance answers the running one's. A different answer
  // means the server restarted, and maybe upgraded, since this page loaded: the page may be an
  // old UI over a new server. The version cannot say so (it does not move with the code); the
  // process can. Asked once a minute while the tab is visible, and at once when it becomes
  // visible again, which is when a person comes back to it after a restart. A failed ask is no
  // news (the server may be restarting), so it is silent and asked again. Once moved, the notice
  // stays, is said once in #status, and nothing is asked again.
  const INSTANCE_EVERY_MS = 60000;
  function watchInstance() {
    const meta = document.querySelector("meta[name='gonk-instance']");
    const moved = $("moved");
    if (!meta || !moved || !window.fetch) return;
    const mine = meta.getAttribute("content");
    let done = false;
    let asking = false;
    async function check() {
      if (done || asking || document.hidden) return;
      asking = true;
      try {
        const r = await fetch("/instance", { cache: "no-store", credentials: "same-origin" });
        const now = r.ok ? (await r.text()).trim() : "";
        if (now && now !== mine) {
          done = true;
          clearInterval(timer);
          moved.hidden = false;
          announce(moved.textContent.trim());
        }
      } catch (_) {
        // Not answering is not news.
      } finally {
        asking = false;
      }
    }
    const timer = setInterval(check, INSTANCE_EVERY_MS);
    document.addEventListener("visibilitychange", check);
    // A page restored from the back/forward cache is the page it was, with its old id.
    window.addEventListener("pageshow", (e) => {
      if (e.persisted) check();
    });
  }

  async function init() {
    wireSafeMode();
    watchInstance();
    wireCommentOrder();
    wireSamples();
    wireLedgerMenu();
    wireDashboard();
    checkAllWords();
    focusRefusal();
    // A whole page that came back with a verdict (a Views save): a live region says nothing
    // about what it was parsed with, so the sentence is said now.
    const verdict = document.querySelector("main [data-announce]");
    if (verdict) announce(verdict.textContent);
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
    canSignIn = true;

    const t = token();
    if (t) {
      try {
        const who = await post("/auth/session", t);
        $("auth-who").textContent = "Signed in as " + who.label + " (grant " + who.grant + ")";
        $("auth-who").hidden = false;
        $("auth-logout").hidden = false;
        signedIn = true;
      } catch (_) {
        setToken("", 0);
        login.hidden = false;
      }
    } else {
      login.hidden = false;
    }
    drawNotices(document);
    focusRefusal();

    sayLater();
    const m = location.hash.match(/^#invite=([A-Za-z0-9_-]+)$/);
    if (m) {
      hideFirstPasskey();
      $("enrol").hidden = false;
      const adding = signedIn ? wireEnrolChoice() : () => false;
      $("enrol-form").addEventListener("submit", (e) => {
        e.preventDefault();
        if (adding()) addGrant(m[1]);
        else enrol(m[1], $("enrol-label").value.trim());
      });
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
