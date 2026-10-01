// Setup without a terminal, through the host (docs/engine.md): the
// gateway's accounts, a sign-in with its pasted address, a provider's key,
// and the person's settings -- the model, the effort, the sandbox level,
// the theme. With a session open, its own commands change it (and save);
// with none, the host saves the setting for the sessions to come.

/** The accounts the gateway serves, fetched again. */
export async function loadAccounts(app) {
  app.S.accounts = { ...(app.S.accounts || {}), loading: true, error: "" };
  app.changed();
  try {
    app.S.accounts = { list: await app.engine.accounts(), loading: false, error: "" };
  } catch (e) {
    app.S.accounts = { list: app.S.accounts?.list || [], loading: false, error: String(e?.message || e) };
  }
  app.changed();
}

/** The person's saved settings, fetched again. */
export async function loadSettings(app) {
  try { app.S.settings = await app.engine.settings(); } catch { /* the last ones stay */ }
  app.changed();
}

/** Saves one setting on the host; the error, said plainly, when it is refused. */
export async function saveSetting(app, key, value, said) {
  try {
    await app.engine.setSetting(key, value);
    app.S.settings = { ...(app.S.settings || {}), [key]: value };
    if (said) app.say(said);
    return true;
  } catch (e) {
    app.say(`Not saved: ${String(e?.message || e)}`);
    return false;
  } finally {
    app.changed();
  }
}

/** The model a choice in the Models sheet sets: the session's, or the next sessions'. */
export function chooseModel(app, name) {
  if (!name) return;
  const s = app.cur(), main = app.S.m.role === "main";
  if (s && !s.ended) {
    const was = main ? s.state.facts.model : s.state.facts.subagents;
    if (was === name) return;
    s.control(main ? `/model ${name}` : `/model subagent ${name}`);
    app.say(main ? `Main is now ${name}` : `Every subagent now runs on ${name}`, was ? { label: "Undo", run: () => s.control(main ? `/model ${was}` : `/model subagent ${was}`) } : null);
    return;
  }
  saveSetting(app, main ? "model.parent" : "agents.model", name, main ? `New sessions start on ${name}` : `Subagents run on ${name}`);
}

export function chooseEffort(app, effort) {
  const s = app.cur();
  if (s && !s.ended) {
    const was = s.state.facts.effort;
    if (was === effort) return;
    s.control(`/effort ${effort}`);
    app.say(`Effort is now ${effort}`, was ? { label: "Undo", run: () => s.control(`/effort ${was}`) } : null);
    return;
  }
  saveSetting(app, "session.effort", effort, `New sessions start at ${effort}`);
}

/** The sandbox level: the session's, saved for the next ones too; with none open, saved. */
export function chooseLevel(app, word) {
  const s = app.cur();
  if (s && !s.ended) { s.setLevel(word, true); app.S.settings = { ...(app.S.settings || {}), "sandbox.level": word }; return; }
  saveSetting(app, "sandbox.level", word, null);
}

/** A sign-in to `provider`, run by the host; its sheet shows how it stands. */
export async function startSignIn(app, provider, label) {
  cancelSignIn(app, true);
  app.S.signin = { provider, label: label || provider, link: "", notes: [], state: "starting", account: "" };
  app.S.signinConfirm = false;
  app.S.sheet = "signin";
  app.changed();
  try {
    const run = await app.engine.signIn(provider, (said) => {
      const si = app.S.signin;
      if (!si || si.provider !== provider) return;
      si.state = said.state || si.state;
      if (said.authorize_url) si.link = said.authorize_url;
      if (said.account) si.account = said.account;
      const words = said.message || said.error || said.note;
      if (words) si.notes.push(String(words));
      app.changed();
    });
    app.signInRun = run;
    const done = await run.done;
    if (app.signInRun !== run) return;
    app.signInRun = null;
    const si = app.S.signin;
    if (done.connected) {
      app.S.signin = null;
      app.say(`Signed in${si?.account ? ` as ${si.account}` : ""} · ${si?.label || provider}`);
      if (app.S.sheet === "signin") app.S.sheet = "models";
      loadAccounts(app);
    } else if (si && !si.cancelled) {
      si.state = "ended";
      si.notes.push("The sign-in ended without connecting an account.");
    }
  } catch (e) {
    if (app.S.signin) { app.S.signin.state = "ended"; app.S.signin.notes.push(String(e?.message || e)); }
  }
  app.changed();
}

export function cancelSignIn(app, quiet = false) {
  const run = app.signInRun;
  app.signInRun = null;
  if (run) run.cancel();
  if (app.S.signin) app.S.signin.cancelled = true;
  app.S.signin = null;
  if (!quiet && run) app.say("Sign-in cancelled · no account was added");
}

/** Hands the gateway a key, read from the field and cleared from it at once. */
export async function saveKey(app, provider, field) {
  const key = field?.value?.trim() || "";
  if (field) field.value = "";
  if (!provider) { app.say("Name the provider the key is for"); return; }
  if (!key) { app.say("Paste the key first"); return; }
  app.S.keySaving = true;
  app.changed();
  try {
    const said = await app.engine.setKey(provider, key);
    app.say(`Key saved for ${provider}${said?.stored_in ? ` · kept in the gateway's ${said.stored_in}` : ""}`);
    app.S.sheet = "models";
    loadAccounts(app);
  } catch (e) {
    app.say(`The key was not saved: ${String(e?.message || e)}`);
  } finally {
    app.S.keySaving = false;
    app.changed();
  }
}
