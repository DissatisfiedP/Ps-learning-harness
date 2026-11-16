# P's Learning Harness

A tutor that finds out what you already know, plans a route through a topic, teaches it one
section at a time, and keeps track of what you can actually do afterwards. It is a desktop app
(Tauri: a Rust core with a plain web UI) that brings no model of its own: you connect a provider
by API key or subscription sign-in. Everything it records is plain text files that you own.

## How it teaches

1. **Measure.** You say what you want to learn and why. The tutor probes for the edge of what
   you know (8–15 questions, a 3–5 question quick check, or none if you are new to the field),
   skipping ideas you have already shown you hold.
2. **Plan.** A planner turns that into sections. Each section has a *mode* that fits the
   material (mechanisms are derived step by step, facts get a hook and spaced recall, procedures
   go from worked example to solo task, perceptual skills get many labelled examples, arguments
   are taken apart premise by premise) and a *target level*: Explain, Use, Analyze or Design.
   You can change depth, drop sections, mark parts you already know, or leave a note. Nothing
   is taught until you approve the plan.
3. **Teach.** Per step: you predict, the tutor teaches, a check follows. Every answer carries
   your confidence. Only a passed check closes a section; what you read, or say you know, is
   not counted as evidence. A wrong step is repaired before anything is built on it.
4. **Practice.** A coach writes fresh problems for the ideas you choose or that are due. It
   commits a worked solution before posing a problem, does not name the idea being tested,
   gives hints one rung at a time, and retests a miss in a new form. Problems come one at a
   time, as a set, or as a timed exam paper.
5. **Review.** Each idea has a memory state (FSRS) computed from your answers. When recall is
   likely to have faded, Home offers it for practice as a new problem, not a repeated card.

Around that loop:

- **Map.** The ideas you have met, as a graph grouped by topic with prerequisite edges, each
  idea showing how well it is holding.
- **Hunts.** Where a field has lore or live debate, a section can start with an assignment to go
  and find things out; the tutor debriefs what you bring back before teaching. Only you can
  turn one on.
- **Field guide.** The forums and references where the field lives, and what you need to know
  before each becomes useful.
- **Calibration.** Once there is enough data: "When you picked *Fairly sure*, you were right 9
  of 15 times."
- **Plugins, reference libraries, sync.** Plugins add tools, figures and interactive views to a
  lesson. A folder of PDFs or Office files can be linked for the tutor to cite. Sessions sync
  through a git remote or a WebDAV folder.

The phase rules (no teaching before an approved plan, no closing a section without a passed
check) are enforced in code, not left to the prompt. `docs/learning-system.md` has the reasoning
and the research behind each choice.

## LearnHarness

This started from studying [LearnHarness](https://github.com/amosblomqvist/learnharness)
(v0.4.1, AGPL-3.0). The probe → plan → teach loop, predict-then-check, the practice discipline
and plain local files come from it. No code or text is copied; it is a separate implementation.

What differs: several teaching modes instead of one; a record of what the learner can do, which
drives review and stops the probe re-asking what is known; a graph organised by topic instead of
by session; phase rules enforced in code; and a Tauri shell instead of Electron with a server
process, so the same app can target phones.

`cargo run -p harness --bin import_learnharness` and `--bin port_learnharness_session` bring a
LearnHarness graph and chat over.

## Running it

```sh
nix develop                # or: stable Rust, cargo-tauri and Tauri v2's Linux dependencies
cargo tauri dev
cargo test --workspace     # scripted provider, no network
scripts/ui-check/run.sh    # the UI in headless Chromium
```

Data lives in `~/.local/share/com.pslearningharness.app/repo` (override with
`$LEARNING_HARNESS_REPO`), settings in `~/.config/com.pslearningharness.app`.

### On NixOS

```nix
# flake.nix
inputs.ps-learning-harness.url = "github:DissatisfiedP/Ps-learning-harness";

# configuration
environment.systemPackages = [ inputs.ps-learning-harness.packages.${pkgs.system}.default ];
```

On x86_64 `default` is the release's prebuilt binary linked against your nixpkgs: a ~10 MB
download, nothing compiled, and it works with `inputs.nixpkgs.follows`. `.#source` (and
`default` on aarch64, or before the first release with a Linux binary) builds only the app from
source: ~100 MB of crate sources, ~1.7 GB of temporary build directory, ~2.5 GB of RAM. Both
need WebKitGTK 4.1 (~840 MB with its dependencies, from cache.nixos.org).

## Edge cases

- Keys and sign-ins go to the OS secure store. On a Linux desktop without a Secret Service they
  stay in a `0600` file, and Settings says so.
- A session that used a desktop-only plugin opens read-only on a phone until you choose
  "Continue here anyway". Sessions made by an older tutor open read-only.
- A built-in plugin that needs a permission stays off until you allow it: in Settings, or when
  the tutor or the plan asks for it.
- Course and exam goals never get hunts. Calibration appears only after 20 graded answers.
- The `sandbox` labs need Podman or Docker; `code-runner` and `repl` need the language's
  toolchain on `PATH`.
- Scanned pages have no text (no OCR); `.doc`, `.xls`, `.ppt` and iWork files are skipped.
- Folders cannot be picked on mobile, so workspaces, reference libraries and image sets can
  only be imported on desktop.
- The macOS build is not notarized: run
  `xattr -dr com.apple.quarantine "/Applications/P's Learning Harness.app"` once. The iOS build
  is unsigned and has to be sideloaded. The Windows installer is unsigned: SmartScreen warns on
  the first run ("More info" → "Run anyway").

## Known defects

- It has barely run against a real model. Every agent path is tested with a scripted provider,
  so the quality of the teaching, planning, marking and graph building is unproven.
- The UI has only been checked in headless Chromium, not in WebKitGTK or on a device.
- The Android, iOS and Windows builds have never run on a device.
- Sync has only run against local test servers. On Android, git does not trust certificates you
  installed yourself.
- Secure storage is proven on Linux only.
- A two-finger pinch on a touch screen under WebKitGTK may zoom the whole window.
- The scratch pad has never been used with a real pen.
- An exam timer does not survive an app restart until the next action in that session.
- The Map's confidence line and frontier list come from the newest session only, and a
  resource the graph builder attached to the wrong topic cannot be corrected in the UI.

`docs/unimplemented.md` is the full list; `docs/architecture.md` is the design.
