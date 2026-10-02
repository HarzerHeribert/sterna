// The scripted turns the mock engine plays back. The first turn of a new
// session is the mockup's tally fix; a later turn is a short one; the busy
// scenario's sessions each hold at one state (a question, a running cell, a
// cell being written). Development and tests only.

const P1 = `const run = await bash({ command: "cargo test -p tally", timeout: 120 });
const [src, tests] = await Promise.allSettled([
  read({ path: "crates/tally/src/amount.rs" }),
  read({ path: "crates/tally/tests/amount.rs" }),
]);
const callers = await grep({ pattern: "parse_cents", path: "crates" });
console.log(run.stdout.split("\\n").filter((l) => /FAILED|panicked|left|right/.test(l)).join("\\n"));`;

const P2 = `await edit({
  path: "crates/tally/src/amount.rs",
  old: "    let s = s.trim();\\n",
  replacement: "    let s = s.trim();\\n    let (sign, s) = match s.strip_prefix('-') {\\n        Some(rest) => (-1, rest),\\n        None => (1, s),\\n    };\\n",
});
await edit({
  path: "crates/tally/src/amount.rs",
  old: "Ok(whole * 100 + frac)",
  replacement: "Ok(sign * (whole * 100 + frac))",
});
const run = await bash({ command: "cargo test -p tally" });
console.log(run.stdout.split("\\n").filter((l) => l.startsWith("test result")).join("\\n"));`;

const ANSWER_FIRST = "The failing test passes: `parse_cents` now applies the minus sign to the cents as well as the whole units.";
const ANSWER_ALLOWED = "Before, −12.50 was read as −11.50 and −0.50 as +0.50. The sign is now taken off first and applied to the total, in crates/tally/src/amount.rs. All 14 tally tests pass, and all 61 across the workspace, including report and cli, which call `parse_cents` too.";
const ANSWER_DENIED = "Before, −12.50 was read as −11.50 and −0.50 as +0.50. The sign is now taken off first and applied to the total, in crates/tally/src/amount.rs. All 14 tally tests pass. The other crates' tests did not run: report needs insta from crates.io, and the download was denied. `cargo test --workspace` runs them once crates.io can be reached.";

const P3 = `// report and cli call parse_cents too; their tests have to pass as well.
const run = await bash({ command: "cargo test --workspace", timeout: 300 });
const results = run.stdout.split("\\n").filter((l) => l.startsWith("test result"));
console.log(results.join("\\n"));
if (run.exit_code === 0) {
  answer(${JSON.stringify(ANSWER_FIRST + "\n\n" + ANSWER_ALLOWED)});
}`;

const P4 = `answer(${JSON.stringify(ANSWER_FIRST + "\n\n" + ANSWER_DENIED)});`;

const OUT1 = `test amount::tests::negative_amounts ... FAILED
thread 'amount::tests::negative_amounts' panicked at crates/tally/src/amount.rs:58:9:
assertion \`left == right\` failed
  left: Ok(-1150)
 right: Ok(-1250)`;

const DIFF2 = `--- a/crates/tally/src/amount.rs
+++ b/crates/tally/src/amount.rs
@@ -17,7 +17,11 @@
 pub fn parse_cents(s: &str) -> Result<i64, ParseError> {
     let s = s.trim();
+    let (sign, s) = match s.strip_prefix('-') {
+        Some(rest) => (-1, rest),
+        None => (1, s),
+    };
     let (whole, frac) = s.split_once('.').unwrap_or((s, "0"));
     let whole: i64 = whole.parse()?;
     let frac: i64 = format!("{frac:0<2}")[..2].parse()?;
-    Ok(whole * 100 + frac)
+    Ok(sign * (whole * 100 + frac))
 }`;

const RES3 = `test result: ok. 14 passed; 0 failed; 0 ignored; finished in 0.01s
test result: ok. 21 passed; 0 failed; 0 ignored; finished in 0.03s
test result: ok. 12 passed; 0 failed; 0 ignored; finished in 0.00s
test result: ok. 14 passed; 0 failed; 0 ignored; finished in 0.02s`;

const E1 = "The test is inline in crates/tally/src/amount.rs, not under tests/. `parse_cents` gives the minus sign to the whole units only and adds the cents back as positive, so −12.50 comes out as −1200 + 50 = −1150.";

/** The first turn of a new session: three cells, a question to you, an answer. */
export const tally = [
  { reason: "I'll run the tally tests first, and read the test that fails and the function it tests." },
  {
    cell: {
      description: "Run the tally tests and read the one that fails", code: P1, runFor: 1.4,
      calls: [
        { tool: "bash", arg: "cargo test -p tally", status: "returned" },
        { tool: "read", arg: "amount.rs", status: "returned" },
        { tool: "read", arg: "amount.rs", status: "failed · NotFound" },
        { tool: "grep", arg: "parse_cents", status: "returned" },
      ],
      stdout: OUT1,
    },
  },
  { prose: E1 },
  {
    cell: {
      description: "Carry the minus sign through parse_cents, then rerun the tally tests", code: P2, runFor: 1.2,
      calls: [
        { tool: "edit", arg: "amount.rs", status: "returned" },
        { tool: "edit", arg: "amount.rs", status: "returned" },
        { tool: "bash", arg: "cargo test -p tally", status: "returned" },
      ],
      stdout: "test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s",
      changes: DIFF2,
    },
  },
  { reason: "Two other crates call parse_cents, so I'll run the whole workspace's tests before I call this done." },
  {
    cell: {
      description: "Run every crate's tests: report and cli call parse_cents too", code: P3, runFor: 1.2,
      approval: {
        tool: "bash", arguments: { command: "cargo test --workspace", timeout: "300" },
        label: "bash · cargo test --workspace", target: "cargo test --workspace", confirmation: "cargo test --workspace",
        reason: "Cargo wants to download insta 1.41.1, a test dependency of crates/report/Cargo.toml, to build and run the tests of all four crates. You chose Sandboxed, which asks only to leave the sandbox.",
        hosts: ["static.crates.io"], leaves_sandbox: true,
      },
      calls: [{ tool: "bash", arg: "cargo test --workspace", status: "returned" }],
      stdout: RES3,
      returned: ANSWER_FIRST + "\n\n" + ANSWER_ALLOWED,
      denied: {
        calls: [{ tool: "bash", arg: "cargo test --workspace", status: "denied · static.crates.io" }],
        error: { class: "Refused", message: "you denied this call for the session. Nothing ran." },
        then: { description: "Say what changed and what did not run", code: P4, calls: [], returned: ANSWER_FIRST + "\n\n" + ANSWER_DENIED },
      },
    },
  },
];

/** A call too large to confirm whole: it can be refused, never allowed (docs/engine.md). */
export const tooLarge = [
  {
    cell: {
      description: "Write the generated file", runFor: 0.3,
      code: `await write({ path: "generated.json", content: big });`,
      approval: {
        tool: "write", arguments: { path: "generated.json" }, label: "write generated.json", target: "generated.json",
        confirmation: "write generated.json (4.1 MB, shown in part)", complete: false, reason: "You chose Ask, which asks before every edit and command.",
        hosts: [], leaves_sandbox: false,
      },
      calls: [{ tool: "write", arg: "generated.json", status: "returned" }], returned: "Written.",
      denied: { calls: [{ tool: "write", arg: "generated.json", status: "denied · refused" }], error: { class: "Refused", message: "the call was refused. Nothing ran." }, then: { description: "Say what did not happen", code: `answer("Nothing was written.");`, calls: [], returned: "Nothing was written: the call was refused." } },
    },
  },
];

/** A later turn: one short cell that answers. */
export const followup = (task) => [
  { reason: "One look at the working tree answers this." },
  {
    cell: {
      description: "Read what the working tree holds now", runFor: 0.8,
      code: `const st = await bash({ command: "git status --short" });\nconsole.log(st.stdout);\nanswer("Done.");`,
      calls: [{ tool: "bash", arg: "git status --short", status: "returned" }],
      stdout: " M crates/tally/src/amount.rs",
      returned: `This is the mock engine: it plays back scripted turns and has no model behind it. You asked: “${task.split("\n")[0].slice(0, 160)}”.`,
    },
  },
];

const doneCell = (description, cmd, out) => ({
  cell: { description, runFor: 0.2, code: `const run = await bash({ command: ${JSON.stringify(cmd)} });\nconsole.log(run.stdout);`, calls: [{ tool: "bash", arg: cmd, status: "returned" }], stdout: out },
});

/** The busy scenario's sessions: each stops at one state and holds. */
export const busy = {
  links: [
    doneCell("Find every link in the docs", "rg -o 'https?://[^) ]+' docs", "412 links in 38 files"),
    doneCell("Fix the links that point inside the site", "node scripts/fix-internal-links.mjs", "fixed 9 links"),
    doneCell("Rebuild the sitemap", "npm run sitemap", "wrote dist/sitemap.xml (214 pages)"),
    {
      cell: {
        description: "Check the external links", runFor: 1.5,
        code: `const run = await bash({ command: "node scripts/check-links.mjs" });\nconsole.log(run.stdout);\nanswer("The internal links are fixed and the sitemap is rebuilt; the external links answer.");`,
        approval: {
          tool: "bash", arguments: { command: "node scripts/check-links.mjs" }, label: "bash · node scripts/check-links.mjs",
          target: "node scripts/check-links.mjs", confirmation: "node scripts/check-links.mjs",
          reason: "Cell 4 runs the docs' link checker, which wants to fetch the external links it found. You chose Sandboxed, which asks only to leave the sandbox.",
          hosts: ["github.com"], leaves_sandbox: true,
        },
        calls: [{ tool: "bash", arg: "node scripts/check-links.mjs", status: "returned" }],
        stdout: "403 external links, all answer", returned: "The internal links are fixed and the sitemap is rebuilt; every external link answers.",
        denied: {
          calls: [{ tool: "bash", arg: "node scripts/check-links.mjs", status: "denied · github.com" }],
          error: { class: "Refused", message: "you denied this call for the session. Nothing ran." },
          then: { description: "Say what was fixed and what was not checked", code: `answer("The internal links are fixed and the sitemap is rebuilt. The external links were not checked.");`, calls: [], returned: "The internal links are fixed and the sitemap is rebuilt. The external links were not checked: the link checker needed github.com, and that was denied." },
        },
      },
    },
  ],
  csvdocs: [
    doneCell("Read the export code and the README", "rg -n 'export' crates/report/src", "14 matches"),
    {
      cell: {
        description: "Write one example per format and check each one runs", runFor: 45,
        code: `for (const format of ["csv", "tsv", "json"]) {\n  const run = await bash({ command: \`cargo run -p cli -- export --format \${format} fixtures/ledger.csv\` });\n  console.log(format, run.exit_code);\n}`,
        calls: [{ tool: "bash", arg: "cargo run -p cli -- export --format csv", status: "returned" }],
        stdout: "csv 0\ntsv 0\njson 0", returned: "The README now has one example per format, and each one runs.",
      },
    },
  ],
  ratelimit: [
    doneCell("Read the search route", "rg -n 'search' src/routes", "src/routes/search.ts:12"),
    doneCell("Find how keys are read", "rg -n 'apiKey' src", "src/auth.ts:31"),
    doneCell("Run the tests as they are", "npm test", "48 passed"),
    doneCell("Add the limiter", "npm run build", "built in 2.1 s"),
    { slow: 60 },
    {
      cell: {
        description: "Prove the limit with a test", runFor: 1,
        code: `const run = await bash({ command: "npm test -- search.rate" });\nconsole.log(run.stdout);\nanswer("/search allows 20 requests a minute per key, and a test proves the 21st is refused.");`,
        calls: [{ tool: "bash", arg: "npm test -- search.rate", status: "returned" }],
        stdout: "2 passed", returned: "/search allows 20 requests a minute per key, and a test proves the 21st is refused.",
      },
    },
  ],
  csvexport: [
    { wait: 2 },
    {
      cell: {
        description: "Run the export tests", runFor: 0.5,
        code: `const run = await bash({ command: "cargo test -p report export" });\nconsole.log(run.stdout);\nanswer("CSV export works: report writes every column, quoted where it has to be.");`,
        calls: [{ tool: "bash", arg: "cargo test -p report export", status: "returned" }],
        stdout: "test result: ok. 6 passed", changes: "--- a/crates/report/src/csv.rs\n+++ b/crates/report/src/csv.rs\n@@ -1,3 +1,4 @@\n use std::io::Write;\n+use crate::quote;\n", returned: "CSV export works: report writes every column, quoted where it has to be.",
      },
    },
  ],
};

/** A finished session's record, for the list's sessions that are not running. */
export const history = (title) => [
  {
    cell: {
      description: "Do what was asked", runFor: 0, code: `answer("Done.");`, calls: [],
      returned: `Finished earlier: “${title}”. This record is the mock engine's; it plays back scripted turns.`,
    },
  },
];
