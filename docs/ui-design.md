# Learning Harness UI — "Carrel"

Design direction and implementable spec for reworking `ui/` (HTML/CSS/JS, no bundler, no new
dependencies). It covers every view and state the backend contract can produce. Rust and the
backend contract stay as they are.

How to read this document:

- §1 says what is wrong with the current UI, based on screenshots and measurements.
- §2 summarises the evidence and how strong each part is.
- §3–§4 name the direction and its principles.
- §5–§10 are the spec: tokens, layout, surfaces, motion, interaction, and removals.
- §11–§12 list the constraints and an acceptance checklist you can check with screenshots.
- §14 lists the sources. Inline citations look like `[Author Year]` and resolve there.

---

## 1. Diagnosis: where the "classic AI look" comes from

The current UI was rendered with a mocked `window.__TAURI__` (`/tmp/lh-bench/mock.js`) in headless
Chromium at 1280×860 and 390×844, in light and dark modes. What I saw:

| # | Observation | Why it matters |
|---|---|---|
| D1 | A three-column chat-app frame: a history sidebar (272 px) always visible, the conversation, and a 320 px plan panel open by default on wide screens. | Other sessions and the whole plan stay in peripheral view the entire time. Visible reminders of other work cause self-interruption [Dabbish 2011]. The reading column gets squeezed (see D2). |
| D2 | Prose is 15.5 px sans with line-height 1.7, inside a 760 px column. **Measured: 82 characters per line with the plan panel open (633 px of text). Computed for the full 760 px column: ≈98.** | That is longer than the 45–75 character typographic range, and longer than the ~55 cpl that gave the best comprehension in [Dyson & Haselgrove 2001]. 15.5 px is below the sizes that tested best in [Rello 2016]. |
| D3 | Every tutor reply starts with a gradient avatar plus "Tutor · mock/model". User text sits in right-aligned rounded bubbles. | This is the chat-product signature. It is chrome that repeats on every turn and carries no learning content (coherence principle, [Sundararajan & Adesope 2020]). |
| D4 | The header holds a "LEARN" uppercase pill, the title, a "Plan" button and a model pill with a **glowing green status dot**. Below it sits a separate phase-strip band, so the session has two stacked bars. | Status chrome competes with content. At 390 px **the session title is pushed out completely**, leaving only "LEARN", "Plan" and the model pill. |
| D5 | Cards have uppercase accent kickers ("QUICK QUESTION", "PREDICT FIRST", "YOUR CALL"), header bars, borders and pills ("Correct", "guided", "practice", "Done"). Plugins are "×" chips. | Badges and pills split attention into many small salient objects. |
| D6 | Motion: bouncing typing dots, a blinking caret (`steps(2) infinite`), rotating spinners in job chips and the phase strip, and a toast that slides in. | Onsets and motion capture attention involuntarily [Yantis & Jonides 1984; Abrams & Christ 2003; Franconeri & Simons 2003]. Infinite blinking for more than 5 s also breaks WCAG 2.2.2 unless it can be paused. |
| D7 | The stream follows the bottom (`state.stick`, `scrollToBottom` on every paint). | The text moves under the reader. Re-reading (regressions) supports comprehension [Schotter 2014]. |
| D8 | While a question is pending, the free-text composer stays visible with the placeholder "Answer the question above first…". In the screenshot the question was **below** the fold, not above. | Two input targets, one of which the backend rejects. The placeholder is spatially wrong. |
| D9 | The confidence slider starts at 50 %. | A default value anchors slider answers [Liu & Conrad 2019]. The confidence data the tutor relies on is biased. |
| D10 | `questionCard` renders `role: "check"` cards as a **closed `<details>`, even while they are pending**, so the answer form is hidden. | This is a bug. A pending exercise must never be collapsed. |
| D11 | The tutor's reasoning is shown in a "Reasoning" disclosure above every reply. | The reasoning can contain the answer to the question that comes next, which would show the answer before the learner has answered (the contract forbids that). It is also a seductive detail. |
| D12 | Errors and rejections appear as toasts that disappear after 2.6–6 s. | The message vanishes, possibly before it is read. It is also an onset in the periphery. |
| D13 | Indigo/violet accent (`#7c8cff`, `#5b6cff`) plus a purple gradient (`#b07cff`). | This is the house palette of generic AI products. |

Most of D1–D13 are not visual taste issues. They are attention costs. The rework removes them
rather than restyling them.

---

## 2. Evidence summary

Strength labels: **Strong** means meta-analyses or many replications. **Moderate** means
consistent lab findings with clear boundary conditions. **Weak/contested** means mixed results or
small samples. I read abstracts and summaries, not every full text (see §13).

### 2.1 Attention capture by onsets, motion and peripheral change — *Moderate to strong*
- Abrupt onsets (new objects appearing) capture attention even when they are irrelevant to the task [Yantis & Jonides 1984].
- Motion onset captures attention [Abrams & Christ 2003; Smith & Abrams 2018].
- Moving and looming objects capture attention; receding ones do not [Franconeri & Simons 2003].
- Contested detail: motion onset did *not* capture attention when the following motion was smooth (high refresh) [Sunny & von Mühlenen 2011], and the debate continues [Smith & Abrams 2018]. What is not contested: abrupt appearance and jerky or looping change in the periphery are the risky cases.
- Peripheral motion is detected very reliably, which is exactly why it distracts from a primary task [Bartram 2003].
- **Implication:** nothing outside the reading point may appear, blink, spin or slide. Change happens in place, statically, and preferably at moments when the learner has already shifted attention (§2.2).

### 2.2 Interruption cost, resumption lag and breakpoints — *Strong*
- Interruptions of only ~2.8 s doubled sequence errors in the interrupted task; ~4.4 s interruptions tripled them [Altmann 2014].
- After an interruption, resuming takes time (the resumption lag). Cues that stay available reduce it [Altmann & Trafton 2004].
- In the field, alerts pull people into other applications, and some suspended work is never resumed [Iqbal & Horvitz 2007]. People compensate for interruptions by working faster, at the cost of more stress and effort [Mark 2008].
- The same interruption costs less at a coarse task boundary than in mid-chunk [Adamczyk & Bailey 2004]. Deferring notifications to breakpoints reduced frustration [Iqbal & Bailey 2008].
- A notification that is merely received, and not acted on, already harms performance on an attention task [Stothart 2015]. Batching notifications improved attention and stress in a field RCT [Fitz 2019].
- People also interrupt themselves, and environmental cues trigger it [Dabbish 2011].
- **Implication:** background events (research, planning, titles) never interrupt. They update in place. Anything that needs the learner is surfaced at a turn boundary, in one fixed place (the desk, §6.3). The session view shows no other sessions. Every return to a session (open, plan sheet closed, error recovered) shows a resumption cue: the current section label and the last tutor turn aligned to the top.

### 2.3 Cognitive load: extraneous load, split attention, redundancy, coherence — *Strong (with boundary conditions)*
- Working memory is narrow. Design should reduce extraneous load [Sweller 2019].
- Physically or temporally separating mutually referring information hurts learning, especially for complex material [Ginns 2006].
- Interesting but irrelevant additions (seductive details) reduce learning: g ≈ −0.33 over 58 studies [Sundararajan & Adesope 2020]. Distraction from visual surroundings has been observed directly in a classroom study, though with young children and a small sample, so generalisation is limited [Fisher 2014].
- "Desirable difficulty" through *perceptual* disfluency (hard-to-read fonts, low contrast) does not help: the meta-analytic effect is null [Xie 2018]. The difficulty must come from the task, never from the typography.
- **Implication:** one column. The question sits in the flow, directly after the text it asks about (contiguity). The plan is a reference you open on demand. No avatars, no per-turn metadata, no decorative chrome. Maximum legibility.

### 2.4 Flow conditions — *Moderate*
- Flow correlates with perceived challenge–skill balance. Clear goals, unambiguous feedback and a sense of control also correlate [Fong 2015]. These are correlations, mostly from self-report. A UI cannot create flow, only avoid blocking it.
- **Implication:** make the *current goal* visible: each section opener shows the section's exit goal, and the running head shows the section label. Make *feedback* immediate and unambiguous: the verdict appears in place, as words, next to the learner's own confidence. Give *control*: the learner scrolls, the learner can stop, and nothing moves on its own.

### 2.5 Reading typography — *Mixed: some strong, some contested*
- **Size:** larger sizes (18 pt and up in their setup) improved readability and comprehension for 104 readers. Extreme line spacings (0.8 and 1.8) hurt [Rello 2016]. Viewing distance changes the absolute numbers, so treat this as "bigger than the 14–16 px habit". *Moderate.*
- **Measure:** ~55 cpl gave the best comprehension at normal and fast reading speeds in one study [Dyson & Haselgrove 2001]. The review literature is mixed: longer lines are sometimes read faster, while moderate lengths are preferred [Dyson 2004]. Typographic practice puts it at 45–90 characters [Butterick, line length]. *Moderate/contested.* I target ~60–72 characters.
- **Serif vs sans:** serifs themselves make no reliable legibility difference [Arditi & Cho 2005]. The choice is free on evidence. Here a serif is chosen for *register* (it separates the tutor's text from the UI) and for harmony with KaTeX's Computer Modern-style math. It is not chosen for speed.
- **Polarity:** dark-on-light (positive polarity) gives better acuity and proofreading in both younger and older adults [Piepenbrock 2013]. Light-on-dark is worst for glance legibility in a dark room [Dobres 2017]. Users still want dark themes at night, and "dark mode reduces eye strain" claims are not well supported. *Moderate for legibility; weak for comfort claims.* Decision: follow the OS setting, with a manual override, and tune the dark theme for contrast. Neither is the "focus" theme.
- **Screen reading:** comprehension of expository text is slightly worse on screens than on paper (g ≈ −0.21), more so under time pressure [Delgado 2018]. *Moderate.* This supports a self-paced, stable page with no pressure cues (timers, ticking counters).
- **Regressions:** readers who cannot look back understand less [Schotter 2014]. *Moderate.* The text must hold still.

### 2.6 Retrieval practice, generation, prequestions, confidence — *Strong (core) / contested (confidence reactivity)*
- Practice testing beats restudy, with and without feedback [Adesope 2017]. Generating an answer beats reading it, g ≈ 0.40 [Bertsch 2007].
- Unsuccessful retrieval attempts still improve later learning when the answer follows [Kornell 2009]. Prequestions before instruction help [Pan & Carpenter 2023].
- Interpolated tests reduce mind wandering in online lectures [Szpunar 2013]. Questions inside the flow are themselves an attention tool.
- High-confidence errors are corrected *more* often after feedback (hypercorrection) [Butterfield & Metcalfe 2001]. Confidence elicitation therefore has learning value when the verdict is shown *next to* the stated confidence.
- Contested: asking for confidence can itself change performance, helping confident learners and hurting unconfident ones [Double & Birney 2019]. The tutor needs the signal, so keep it, but keep it lightweight and neutral.
- Defaults: a slider's starting value anchors answers [Liu & Conrad 2019], and defaults sway choices in general [Johnson & Goldstein 2003].
- **Implication:**
  - Never reveal `args.answer` before an answer exists.
  - Free recall is a typed field, not a list to recognise from.
  - The confidence scale has **no default**.
  - "I don't know" is allowed but secondary, and the copy invites a guess first.
  - The verdict is shown together with "you were *Fairly sure*".
  - A "recommended" choice option is labelled but never preselected.

### 2.7 Progress visibility and the goal gradient — *Moderate / mixed*
- Effort accelerates as a goal gets closer. Perceived progress alone accelerates it too [Kivetz 2006].
- Progress indicators in web surveys have *no overall effect* on completion. Indicators that look slow early *increase* drop-off [Villar 2013].
- **Implication:** show progress at the granularity that moves honestly and visibly: sections ("Section 2 of 5"), with completed nodes checked in the plan sheet. No percentage bar, no per-message progress, no streaks.

### 2.8 Hick's and Fitts's laws for the remaining controls — *Strong for the law, modest for design gains*
- Choice time grows with the log of the number of equally likely alternatives [Hick 1952]. Movement time grows with distance over target width [Fitts 1954].
- **Implication:**
  - At most one primary action is visible at any moment.
  - Secondary actions are text buttons.
  - Targets are at least 24×24 CSS px (WCAG 2.5.8 AA) everywhere, and at least 44 px tall on touch.
  - Submit sits directly after the last field of its form.
  - The desk action sits at the end of the input it acts on.

### 2.9 Calm technology and ambient information — *Conceptual (design framework, not experiment)*
- Information should move between the *center* and the *periphery* of attention, and live mostly in the periphery [Weiser & Brown 1996]. This framework rests on argument, not experiment. It is used here as vocabulary, backed by §2.1–§2.2.
- **Implication:** the running head and the desk are the periphery. They change rarely, in place, without motion. The page is the center.

---

## 3. Direction: **Carrel**

A *carrel* is the enclosed desk in a library reading room: one lamp, one page, walls on three
sides. The name says what the UI is for. It is not "minimal" in the decorative sense. It is
**enclosure**: whatever is not the task in front of you is out of view, and nothing outside your
line of sight moves.

**Rationale.** The evidence does not reward emptiness for its own sake. It punishes specific
things: onsets and motion in the periphery (§2.1), interruptions mid-chunk (§2.2), extraneous and
split material (§2.3), text that moves under the reader (§2.5), and answers revealed before
effort (§2.6). It rewards specific things too: in-flow retrieval questions, honest coarse progress,
clear goals and immediate feedback (§2.4, §2.6, §2.7). Carrel is the shape that follows from
both lists:

- a **book-like page** for the tutor's words, typographically distinct from the UI;
- the learner acting at **one place at a time**, either the exercise in the flow or the desk at the bottom, never both;
- **two quiet edges** (a running head and a desk) that only change at turn boundaries;
- **everything else behind a deliberate action**: sessions, plan, settings, research notes.

### 3.1 What the screen is for at each moment

| Moment | Center (the one thing in focus) | Periphery (visible, static) | Out of view |
|---|---|---|---|
| Home | The start question and its input | Session list below the fold; "Map" and "Settings" | — |
| Map | The idea map of one plan, or the list beside it | Running head; desk with the selection and "Practise" | Sessions, the composer |
| Waiting for the first words | The learner's own message, with "Writing…" under it | Running head; desk with Stop | — |
| Tutor writing | The growing text (it never moves) | Running head; desk with Stop | Tutor reasoning (Learn) |
| Reading a finished reply | The reply | Desk composer | — |
| Question or choice pending | The exercise block | Running head; desk note "Answer the question to continue" | Composer (hidden, draft kept) |
| Feedback | Verdict and explanation, in place, under the learner's answer | — | — |
| Planner working | The last turn (re-readable); job line "Planner — drafting" | Head "Planning…"; desk note | Composer |
| Plan approval | The plan document | Head "Plan · awaiting approval"; desk note | Composer |
| New section | The section opener (title and exit goal) | Head "Section n of m" | — |
| Looking up the plan | Plan sheet (modal) | Page dimmed behind | — |
| Error | Inline error block with Retry, at the end of the flow | — | Toasts (removed) |

### 3.2 Removed, hidden, peripheral

- **Removed:** sidebar in sessions; avatars; per-reply model and author metadata; model pill and status dot; mode pills and dots; badges and pills; gradients; drop shadows; spinners; typing dots; caret blink; toasts; suggestion chips; the uppercase accent kickers; the second header band.
- **Hidden until asked:** the session list (Home only), the plan (sheet), the learning map (its own page, reached from Home only), research briefs (disclosure), settings (Home only, plus a link inside model-related errors), tutor reasoning in Practice (closed disclosure after the reply).
- **Peripheral and static:** the running head (back, title, phase position, Plan) and the desk (composer, Stop, or the "continue" note).

---

## 4. Principles

Each principle is binding. The evidence keys refer to §2.

- **P1. One center.** At any moment exactly one region invites action: the desk composer, *or* the pending exercise, *or* the plan document, *or* an open sheet. Everything else is static. (§2.3, §2.8)
- **P2. Nothing moves that the learner did not move.** No autoscroll during streaming. No looping animation anywhere. No sliding-in. The only animation is the open and close of a sheet the learner requested, and it is removed under `prefers-reduced-motion`. (§2.1, §2.5 Schotter)
- **P3. Background work stays in the background.** Job progress updates in place, as text. Anything that needs the learner (plan ready, question pending, failure) is surfaced at a turn boundary in the desk. It never takes focus away from where the learner is reading. (§2.2)
- **P4. The page is a page.** The tutor's words are set like a book: serif, 56 ch measure (~67–71 characters per line), 20 px, line-height 1.55, ragged right. The learner's words are set as notes: sans, indented, labelled "You". There are no bubbles, cards or avatars. (§2.3, §2.5)
- **P5. Effort before answers.** Exercises invite generation (typed answer first, "your best guess is fine"). The answer is never in the DOM before submission. Confidence has no default. Verdicts appear beside stated confidence, in neutral ink, never in alarm red. (§2.6)
- **P6. Goals and progress at honest granularity.** Each section opens with its title and exit goal. The head shows "Section n of m". There are no percentages or meters. (§2.4, §2.7)
- **P7. Resumption cues everywhere.** Opening a session, closing the sheet, recovering from an error or answering a question all leave the learner at the start of the relevant turn, with the section label visible. (§2.2 Altmann & Trafton)
- **P8. Legibility over style.** WCAG AA contrast at minimum (most text is AAA), no disfluent type, no thin weights, rem-based sizes, reflow down to 320 px. (§2.3 Xie, §2.5, WCAG)
- **P9. Few controls, big enough, near the thing they act on.** At most one primary button visible. Text buttons for secondary actions. At least 24 px targets, at least 44 px on touch. (§2.8)
- **P10. Messages stay until read.** Errors and statuses are inline at their source and persist until the condition changes. There are no timed toasts. (§2.2, WCAG 2.2.1/4.1.3)

---

## 5. Design tokens

All tokens are CSS custom properties on `:root` in `styles.css`. Use `rem` for sizes, so the root
size scales everything (WCAG 1.4.4).

### 5.1 Font stacks (system fonts only; no downloads)

```css
--font-read: "Iowan Old Style", "Charter", "Bitstream Charter", "Sitka Text",
             "Noto Serif", "Source Serif 4", Cambria, Georgia, "DejaVu Serif", serif;
--font-ui:   system-ui, -apple-system, "Segoe UI", Roboto, "Noto Sans",
             "Helvetica Neue", Arial, sans-serif;
--font-mono: ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas,
             "DejaVu Sans Mono", "Liberation Mono", monospace;
```

Resolution per platform:

- macOS/iOS → Iowan Old Style or Charter.
- Windows → Sitka Text, else Cambria.
- Android → Noto Serif.
- The developer's NixOS machine has Noto Serif, DejaVu Serif and Liberation Serif. Noto Serif resolves there; I checked with `fc-list`.

Never use a weight below 400 for text. UI text uses 400/600 only.

Where each stack is used:

- **`--font-read`:** tutor prose, exercise prompts, the plan document, section openers, the Home display line.
- **`--font-ui`:** the running head, the desk, the learner's words, form controls, labels, job and notice lines, Settings.

### 5.2 Type scale

| Token | Size (desktop ≥ 600 px) | Size (phone < 600 px) | Line-height | Font / weight | Use |
|---|---|---|---|---|---|
| `--fs-meta` | 0.8125rem (13 px) | 13 px | 1.4 | ui 400 | Group headings, small-caps labels. Never body copy. |
| `--fs-small` | 0.875rem (14 px) | 14 px | 1.45 | ui 400 | Help text, job lines, notices, verdict meta |
| `--fs-ui` | 1rem (16 px) | 16 px | 1.45 | ui 400/600 | Controls, running head, desk, Settings body |
| `--fs-learner` | 1.0625rem (17 px) | 16 px | 1.5 | ui 400 | Learner entries, learner answers |
| `--fs-read` | 1.25rem (20 px) | 1.125rem (18 px) | 1.55 / 1.5 | read 400 | Tutor prose, prompts |
| `--fs-h3` | 1.25rem (20 px) | 18 px | 1.35 | read 600 | Prose `h3`–`h6` |
| `--fs-h2` | 1.5rem (24 px) | 1.3125rem (21 px) | 1.3 | read 600 | Prose `h2` |
| `--fs-h1` | 1.75rem (28 px) | 1.5rem (24 px) | 1.25 | read 600 | Prose `h1`, section opener title, plan title |
| `--fs-display` | 2.25rem (36 px) | 1.75rem (28 px) | 1.15 | read 400 | Home question only |
| code | 0.85em of context | same | 1.5 | mono 400 | `pre code`; inline `code` 0.88em |
| math | `.katex { font-size: 1.1em }` | same | — | KaTeX | Override KaTeX's default 1.21em so math matches the serif body |

**Measure:**

- `--measure: 56ch`, applied as `max-inline-size` on the page column. Because the column's `font-family` is `--font-read`, `ch` tracks the serif in use.
- **Measured in Chromium at 20 px:**
  - 56 ch → Noto Serif 626 px / **67 cpl**; DejaVu Serif 713 px / **71 cpl**; Liberation Serif 560 px / **71 cpl**.
  - For comparison, 60 ch gave 72–76 cpl, which is too long. That is why the token is 56 ch, not 60.
- **Acceptance range: 58–72 characters per line** for a body paragraph on desktop (§12).
- On phones the measure is limited by width. At 358 px of text and 18 px it is 39–51 cpl (measured), which is acceptable for phone reading.

Prose details:

- `text-align: start`. Never justify.
- `hyphens: auto` with `lang="en"`, phone only.
- `text-wrap: pretty` on paragraphs and `text-wrap: balance` on headings. Both are progressive enhancements; browsers without support ignore them.
- Paragraph spacing: `margin-block: 0 1em`.
- Heading spacing:
  - `h1`/`h2`: `margin-block: 1.6em 0.5em`.
  - `h3`: `1.3em 0.4em`.
- Lists: `padding-inline-start: 1.3em`, with `li + li { margin-top: 0.3em }`.
- Small caps (used for labels such as "You" and "Section 2 of 5"): `font-variant-caps: all-small-caps; letter-spacing: 0.04em`. Never `text-transform: uppercase`.

### 5.3 Color

Each theme has a single accent ("lamp green"), used only for focus rings, the one primary button,
links and the selected state of choice controls. Verdicts and progress use ink, not color. Every
state is also conveyed in text or glyph (WCAG 1.4.1).

Contrast ratios below are WCAG 2.x relative-luminance ratios, computed with the WCAG formula.

**Light ("paper")**

| Token | Value | Use | vs `--paper` | vs `--raised` |
|---|---|---|---|---|
| `--paper` | `#FBFAF7` | Page background | — | — |
| `--raised` | `#F2F0EA` | Pending exercise block, code blocks, inline code, sheet background | 1.09 (decorative) | — |
| `--ink` | `#1C1B19` | Body text, headings | **16.49** | 15.10 |
| `--ink-2` | `#45423D` | Learner text, secondary text | **9.58** | 8.78 |
| `--ink-3` | `#6A665F` | Meta, labels, placeholders, job lines | **5.47** | 5.01 |
| `--rule` | `#E3DFD6` | Decorative hairlines only (never the only boundary of a control) | 1.27 | 1.17 |
| `--rule-strong` | `#8A857C` | Control borders, exercise block border, learner-entry rule | **3.51** | 3.22 |
| `--accent` | `#2D5B4C` | Focus ring, primary button fill, links, checked radio | **7.42** | 6.79 |
| `--accent-hover` | `#234A3E` | Primary button hover | — | — |
| `--on-accent` | `#FBFAF7` | Text on primary button | 7.42 on accent; 9.49 on hover | — |
| `--danger` | `#A2341F` | Error *text* and error rule (system failures only, never verdicts) | **6.60** | 6.05 |
| `--select` | `#DCE8E2` | `::selection` background | ink on it 13.67 | — |
| `--scrim` | `rgb(28 27 25 / 0.32)` | `<dialog>::backdrop` | — | — |

**Dark ("lamp-lit")**

| Token | Value | vs `--paper` | vs `--raised` |
|---|---|---|---|
| `--paper` | `#191816` | — | — |
| `--raised` | `#23221F` | 1.12 (decorative) | — |
| `--ink` | `#E9E5DD` | **14.12** | 12.66 |
| `--ink-2` | `#C2BCB1` | **9.40** | 8.43 |
| `--ink-3` | `#9A948A` | **5.89** | 5.29 |
| `--rule` | `#34322E` | 1.39 (decorative) | 1.24 |
| `--rule-strong` | `#77726A` | **3.72** | 3.33 |
| `--accent` | `#8CC4AE` | **8.97** | 8.05 |
| `--accent-hover` | `#A6D4C1` | — | — |
| `--on-accent` | `#14201B` | 8.48 on accent; 10.23 on hover | — |
| `--danger` | `#F0A08C` | **8.54** | 7.66 |
| `--select` | `#2A3A33` | ink on it 9.54 | — |
| `--scrim` | `rgb(0 0 0 / 0.5)` | — | — |

Notes:

- Dark text is `#E9E5DD`, not pure white, and the background is `#191816`, not pure black. Some practitioners claim this reduces halation. That claim is weak; the values are chosen to stay well above AAA while avoiding maximum luminance jumps.
- Theme choice is set via `color-scheme` plus `prefers-color-scheme`. A manual override in Settings → Appearance (System / Light / Dark) sets `data-theme` on `<html>` and persists to `localStorage` key `lh.theme`. This is UI-only state; no backend change.
- `forced-colors: active`: all control borders use `ButtonBorder`/`CanvasText`, and focus uses `Highlight`. Do not set `forced-color-adjust: none` anywhere.

### 5.4 Spacing, radii, borders, shadows, layout constants

```css
--s1: 4px; --s2: 8px; --s3: 12px; --s4: 16px; --s5: 24px; --s6: 32px; --s7: 48px; --s8: 64px; --s9: 96px;
--r: 4px;                   /* the only radius: controls, exercise block, code, sheet corners */
--hair: 1px solid var(--rule);
--line: 1px solid var(--rule-strong);
--focus: 2px solid var(--accent);   /* outline, outline-offset: 2px */
--head-h: 48px;  /* phone: 44px */
--desk-min: 64px;
--gutter: 24px;  /* phone: 16px */
```

Rules:

- **Radii:** `0` or `--r`. No pills, no `999px`, no circles except native radio and checkbox glyphs.
- **Shadows:** none anywhere. Dialogs and sheets are separated by the backdrop scrim and a `--line` border.
- **Spacing:**
  - Between turns (tutor reply ↔ learner entry): `--s7`.
  - Between blocks inside a turn (reply → exercise, reply → job line): `--s6`.
  - Section opener: `--s8` above, `--s6` below.
- **Breakpoints:** phone `< 600px`, tablet `600–1023px`, desktop `≥ 1024px`. The layout is one column at every width; breakpoints change only sizes, the plan-sheet placement and the running-head density.

### 5.5 Motion tokens

```css
--dur-sheet-in: 180ms;  --ease-out: cubic-bezier(0.2, 0, 0, 1);
--dur-sheet-out: 120ms; --ease-in:  cubic-bezier(0.4, 0, 1, 1);
--dur-hover: 120ms;     /* color/background only */
```

See §8 for where these may be used.

---

## 6. Global layout and shared components

### 6.1 Structure (session view)

```
┌──────────────────────────── viewport ────────────────────────────┐
│        ┌──────── running head (column-aligned, 48px) ─────────┐  │
│        │ ← Sessions   Derivatives        Measure·Plan·Teach 2/5·Close  Plan │
│        └──────────────────────────────────────────────────────┘  │
│        ┌──────────── page column: 56ch, centered ─────────────┐  │
│        │                                                      │  │
│        │  SECTION 2 OF 5                                      │  │
│        │  The power rule                                      │  │
│        │  Goal: derive the power rule for n = 2, 3            │  │
│        │                                                      │  │
│        │  Tutor prose in serif, 20/31, ragged right …          │  │
│        │                                                      │  │
│        │  │ YOU                                               │  │
│        │  │ Learner's words in sans, ink-2                    │  │
│        │                                                      │  │
│        │  Research — Common misconceptions about limits · done ▸ │
│        │                                                      │  │
│        │  ┌ PREDICT FIRST (small caps) ────────────────────┐  │  │
│        │  │ Prompt in serif …                              │  │  │
│        │  │ Your answer [__________________________]       │  │  │
│        │  │ Your reasoning (optional) [____________]       │  │  │
│        │  │ How sure are you?  ○Guessing ○Unsure ○Even …   │  │  │
│        │  │ [Submit]  I don't know — show me               │  │  │
│        │  └────────────────────────────────────────────────┘  │  │
│        └──────────────────────────────────────────────────────┘  │
│        ┌──────────── desk (column-aligned) ───────────────────┐  │
│        │ Answer the question to continue.     [Go to question] │  │
│        └──────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────────┘
```

- The scroller is `#messages` (keep the id). The running head and the desk are outside the scroller; they are sticky siblings in a grid `grid-template-rows: auto 1fr auto`.
- The running head, page and desk share one inline axis: `inline-size: min(var(--measure) + 2*var(--gutter), 100%)`, centered. The eye never has to travel to the viewport corners.
- Backgrounds:
  - The page is `--paper` edge to edge.
  - The running head is `--paper` with a bottom `--hair`.
  - The desk is `--paper` with a top `--hair`.
- Set `scroll-padding-block: var(--head-h) calc(var(--desk-min) + 16px)` on `#messages`, so focused elements are never hidden under the head or desk (WCAG 2.4.11).

Phone (390 px):

```
┌──────── 390 ────────┐
│ ←  Derivatives  2/5 Plan │  44px head; title truncates; phase shows current only
├─────────────────────┤
│ SECTION 2 OF 5      │
│ The power rule      │
│ Tutor prose 18/27   │
│ …                   │
│ │ YOU               │
│ │ learner text      │
├─────────────────────┤
│ [Reply to the tutor ] [Send] │  desk; padding-bottom: env(safe-area-inset-bottom)
└─────────────────────┘
```

Use `height: 100dvh` so the desk stays above the on-screen keyboard.

### 6.2 Running head

Contents, left to right:

1. **Back button.** Text "← Sessions" on desktop. On phone, a 44×44 icon button `aria-label="Back to sessions"`. It goes to Home. While a Practice reply is streaming, navigation stays blocked as today: show inline status text in the head, "Wait for the reply to finish, or stop it.", for as long as the condition holds. No toast.
2. **Title.** An `h1`, `--fs-ui` 600, `--ink-2`, single line with ellipsis. It must remain visible at 320 px. It may shrink to one word plus ellipsis, but it is never dropped (fixes D4).
3. **Phase position.** A `nav aria-label="Session progress"` containing an `ol` of the four phases. Learn only, not read-only.
   - **Desktop/tablet** shows all four as text: `Measure · Plan · Teach 2/5 · Close`.
     - Past phases: `--ink-2`.
     - Current phase: `--ink` 600 with `aria-current="step"`.
     - Future phases: `--ink-3`.
     - Separators are `·` with `aria-hidden`.
   - **Phone** shows the current phase label only; the other three are `.sr-only`.
   - Label rules:
     - Measure → "Measure".
     - Plan while the planner is running → "Planning…". This is static text, no spinner; it keeps `role="status"` from today so screen readers hear it once.
     - `awaitApproval` → "Plan · awaiting approval".
     - Teach → "Teach 2/5", or "Teach" when there are no sections.
     - Close or ended → "Close".
4. **Plan button.** A text button "Plan", only when a plan exists and the phase is teach, close or ended (today's `panelAvailable`). It has `aria-haspopup="dialog"` and opens the plan sheet (§7.2.9). Its accessible name includes the position, e.g. "Plan (Teach, section 2 of 5)".

The head never shows the model, mode pills, status dots or icons other than the back arrow.

Practice: back button, title, and the static word "Practice" in `--ink-3` where the phase sits.

### 6.3 Desk (bottom edge)

The desk is the *only* fixed place where the learner acts outside exercises. It has exactly one
state at a time:

| State | When | Content |
|---|---|---|
| **Composer** | Learn: measure or teach, nothing pending, not read-only. Practice: always. | Auto-growing textarea (`min-height` 1 line, `max-height: max(30dvh, 160px)`) with a visible-to-AT label "Reply to the tutor" (`aria-label`, or a `.sr-only` `<label>`) and placeholder "Reply to the tutor". The trailing button reads "Send", primary, 40 px tall (44 px on phone). |
| **Composer + Stop** | A tutor turn is streaming (Learn) or Practice is busy. | The textarea stays editable for drafting. Enter does not send (unchanged). The button reads "Stop" (secondary style, `aria-label="Stop the reply"`) and calls `cancel_reply` (Learn) or the existing Practice stop. |
| **Note: exercise pending** | A `question_ask` or `choice_ask` is pending. | Text "Answer the question to continue." plus a secondary button "Go to question", which scrolls the exercise's first field into view (`block: "center"`, instant) and focuses it. The composer element is hidden (`hidden`), not cleared; its draft is kept. |
| **Note: plan** | `phase === "plan"` with the planner running. | "The planner is drafting your plan. You can keep reading." No button. |
| **Note: approval** | `phase === "awaitApproval"`. | "Review the plan to continue." plus the secondary button "Go to plan". |
| **Read-only** | `SessionSummary.readOnly`. | "This session was made with an earlier version of the tutor and can't continue." plus the primary button "Start a new session" (goes to Home in Learn mode). |

- A rejected send (`send_learner_message` rejects) puts the text back in the textarea, as today. The rejection message appears directly under the textarea in `--danger` text, `--fs-small`, with `role="status"`. It stays until the next input event or the next successful send.
- There is no visible "Enter to send / start" hint anywhere, on Home or in sessions. The keys are standard; they are exposed only as `aria-describedby` text on the input (`.sr-only`).
- Hiding the composer follows today's backend rules exactly: anything the backend would reject (pending card, plan phase, read-only) gets a note instead of an input.

### 6.4 Flow entries (inside the page column)

| Item kind (`timeline.js`) | Rendering |
|---|---|
| `assistant` (tutor) | Bare prose (`.prose`, `--font-read`). No label, avatar, model or time. An `ending` adds a final line in `--fs-small` italic `--ink-3`: `interrupted` → "Stopped here.", `max-tokens` → "The reply was cut off at the length limit.", `refusal` → "The tutor declined to continue." Empty `content` → not rendered (unchanged). |
| `user` (learner) | Block with `border-inline-start: 2px solid var(--rule-strong)` and `padding-inline-start: --s4`. A small-caps "You" label (`--fs-meta`, `--ink-3`), then the text in `--fs-learner` `--ink-2`, `white-space: pre-wrap`. Left-aligned in the column; never right-aligned. |
| `question` | §7.2.4 |
| `choice` | §7.2.5 |
| `job` | §7.2.8 |
| `notice` (plan-notice) | §7.2.7 |
| `divider` (compaction) | A centered line with `--hair` rules either side and the text "Earlier turns summarized" in small caps `--fs-meta` `--ink-3`. `role="separator"`. |
| **`section`** (new item kind, UI-only) | §7.2.6 |

Live reply (`#live`):

- It uses the same `.prose` with no caret pseudo-element.
- Before the first text it shows the static line "Writing…" (`--fs-small`, `--ink-3`, italic). This replaces `TYPING`, the bouncing dots.
- If the view joined a turn already in progress (`midway`), show instead "The tutor is writing. The reply will appear when it's finished."
- If no text arrives within 30 s of `turnStart`, append once, without animation: "Still working. You can stop it below."

### 6.5 Buttons and fields

- **Primary:** fill `--accent`, text `--on-accent`, 600 weight, `--r` radius, 40 px tall (44 px on touch, via `@media (pointer: coarse)`), 0 16 px padding. At most one primary button visible per screen state.
- **Secondary:** transparent fill, `--line` border, `--ink` text.
- **Text button:** no border or fill, `--ink-2` text, underline on hover and focus. Minimum 24 px tall with at least 8 px spacing; 44 px on touch.
- **Fields:** `--paper` fill (or `--raised` when outside the exercise block), `--line` border, `--r` radius, `10px 12px` padding, `--font-ui` 16 px (avoids mobile zoom), visible `<label>` above. Fields grow with content. No `resize` handles except plan-change textareas.
- **Focus:** every interactive element shows `outline: var(--focus); outline-offset: 2px` on `:focus-visible`. Never `outline: none` without this replacement.
- **Disabled:** `opacity: 1`, text `--ink-3`, border `--rule`, plus `aria-disabled` or `disabled`. Never fade below contrast.

---

## 7. Surfaces

### 7.1 Home ("the desk you sit down at")

Desktop:

```
 Learning Harness                                         Map   Settings
 (wordmark, --fs-ui 600 --ink-2, plain text)    (text buttons)

            What do you want to learn?          ← --fs-display, serif, left-aligned in column
            ( ) Learn — the tutor finds what you know, plans, then teaches
                step by step.
            ( ) Practice — problems that hide the idea mid-solution, hints one
                rung at a time, honest grading.
            ┌────────────────────────────────────────────┐
            │ Describe a topic or a goal…                 │ [Start]
            └────────────────────────────────────────────┘
            (no hint line, no suggestions: the question and the input are the whole start area)

            ─────────────────────────────────────────────
            Your sessions
            TODAY
              Derivatives                          Learn · 10:42      Delete
              Long session (500 messages)          Learn · 09:15      Delete
            YESTERDAY
              Chain rule drills                    Practice · Tue     Delete
            OLDER
              Old learn session                    Read-only · 1 Jan  Delete
```

- **Column:** the same 56 ch column as the session page. The top bar holds the wordmark (plain text, no logo mark) and two text buttons, "Map" (§7.6) and "Settings". "Map" is hidden when there is no app backend. Vertical offset: `--s9` above the display line on desktop, `--s7` on phone.
- **Mode:** a real radio group (`fieldset` + `legend` "Mode", visually hidden legend), replacing `role="tablist"` (wrong semantics). Each option shows the label (`--fs-ui` 600) and its one-line description (`--fs-small`, `--ink-3`, linked via `aria-describedby`). Switching mode updates the display line and placeholder from today's `COPY`.
- **Input:** the same `#composer` element moved into the home slot (keep today's slot mechanism). Its label is the display line (`aria-labelledby` = `#hero`). The button reads "Start" (primary).
- **No suggestions.** There is no "Or try:" line. Example topics are content the learner did not ask for (a seductive detail, §2.3) and a second set of targets next to the one input (§2.8).
- **Setup notice** (no model ready): a paragraph above the input, "Connect a model to start." followed by the text button "Open settings". Start stays enabled. Pressing it without a model shows the inline status "Choose a model first." under the input and opens Settings (today's toast becomes inline).
- **Session list** under the heading "Your sessions" (`h2`, `--fs-ui` 600):
  - Groups Today / Yesterday / Previous 7 days / Older, as small-caps `h3` in `--fs-meta` `--ink-3`.
  - Each row is an `li` containing an open button (title in `--fs-ui` `--ink`; meta "Learn · 10:42", "Practice · Tue" or "Read-only · 1 Jan" in `--fs-small` `--ink-3`, right-aligned) and a sibling "Delete" text button.
  - Row height is at least 48 px.
  - Delete is visible on `:hover`/`:focus-within` for `(hover: hover)` devices, and always visible on `(hover: none)`.
  - First press arms the button ("Delete? Press again", `aria-live` polite); a second press within 4 s deletes. This is today's armed pattern, restyled as text.
  - Incremental updates stay exactly as today: one entry per `session` event, moved or updated in place; no re-list.
  - Empty state: "No sessions yet. What you start here will be listed." in `--ink-3`.
- **Errors:**
  - `list_sessions` failure: inline under "Your sessions": "Couldn't load your sessions: ⟨message⟩", with a text button "Try again".
  - `subscribe_events` failure: a one-line notice at the top of Home in `--danger` text: "Live updates are unavailable: ⟨message⟩". It persists.
  - Not inside the app (`!available`): the input area is replaced by "Open Learning Harness through its app to use it."

Phone: the same stacked order. The display line is 28 px. Session-row meta wraps under the title. Delete is always visible.

### 7.2 Learn session

#### 7.2.1 Starting (from Home)

- The view switches to the session immediately (unchanged logic in `startLearn`).
- The learner's text appears as the first "You" entry, and "Writing…" appears under it.
- The head shows the title from the `session` event, as today; the phase reads "Measure".
- Focus goes to the desk textarea with `preventScroll`.
- If `start_teaching` rejects before a session exists, return to Home with the text restored. The error appears under the Home input in `--danger` with `role="status"` instead of a toast.

#### 7.2.2 Streaming reply (live)

- Text renders through the existing `LiveReply`: block cache, tail re-render, one paint per rAF. **`onPaint` must not scroll.**
- **Scroll rule:**
  - At `turnStart` (tutor), if the learner is "at the end" (the bottom of the flow is within 120 px of the viewport bottom, today's `nearBottom`) and the live element's top would be below the visible area, scroll **once**, instantly, so the live element's top sits `--s6` below the running head.
  - After that, never scroll during the turn.
  - If the learner is not at the end, do nothing.
- **Jump to end:** when the flow's end is below the viewport *and* the turn has ended, a text button "↓ Continue reading" (`--fs-small`, `--paper` background) appears at the trailing end of the column, resting on the desk's top edge. It is positioned over the page, so showing it never resizes the desk or the scroller. It is static. Activating it scrolls instantly so the first unread block (the top of the last tutor entry, or of the first exercise after it) sits under the head. It is hidden again once the end is in view. It never appears during streaming, so it cannot flicker.
- **Tutor reasoning (`thinking`):** in Learn, do not render it. Skip `paintThinking`, and keep accumulating the string so `finalize` stays correct. Reason: spoiler risk (D11) and coherence (§2.3).
- **Stop:** the desk button "Stop" → `cancel_reply`. The finalised message carries `ending: interrupted` and shows "Stopped here."
- **Overflow anchoring:** keep the default `overflow-anchor: auto` on the scroller at all times, so items inserted or resized *above* the viewport (prepended pages, job lines, verdicts) do not shift what the learner is reading. Only two elements opt out: the reply that is still streaming (`.entry.streaming`; otherwise the browser follows its tail) and `#live`. Keep today's manual compensation in `prependOlder`.
- **One alignment per turn:** a turn can contain several model calls, each persisted as its own message (e.g. feedback, then `verdict_record`, then more text). Only `turnStart` aligns; a later segment of the same turn starts its reply without scrolling.
- **Reserved space:** the space reserved under an aligned reply (`#live`'s minimum height, so the reply's start can sit under the head) is released as soon as it is below the viewport (at turn end and on scroll). Space in view is never taken away, so nothing moves.

#### 7.2.3 Reply finished

- The live element is finalised in place. The DOM is kept; only the tail is closed (unchanged).
- Nothing else changes visually.
- The screen-reader announcer (§9.6) says "Reply finished."

#### 7.2.4 Question block (`question_ask`)

Pending state is the only element on the page with a fill (`--raised`, `--line` border, `--r`,
padding `--s5`; `--s4` on phone). It is a `<section aria-labelledby>`.

```
 PREDICT FIRST                                  ← small caps, --fs-meta, --ink-3 (h2 visually small)
 Before we compute: what do you predict happens to the secant slope
 of f(x) = x² at x = 1 as h → 0?                ← --font-read, --fs-read
 
 Your answer                                    ← label, --fs-small 600 --ink-2
 [ Your best guess is fine.                       ]  ← textarea, 3 rows; for format:"choice" a radio list (below)
 Your reasoning (optional)
 [ How did you get there?                         ]  ← textarea, 2 rows
 How sure are you?                              ← fieldset legend
 ( ) Guessing  ( ) Unsure  ( ) Even odds  ( ) Fairly sure  ( ) Certain
     ≈10%          ≈30%        ≈50%            ≈70%           ≈90%   ← --fs-meta --ink-3
 [Submit]   I don't know — show me              ← primary + text button; status text after
```

- **Kicker labels** (sentence case rendered as small caps; never uppercase transforms or accent color): `probe` → "Quick question", `predict` → "Predict first", `check` → "Check your understanding".
- **Never collapsed while pending**, for any role. This removes the `details` for `check` (fixes D10).
- **`format: "choice"`:** a `fieldset` with legend "Choose one" (visually hidden, since the prompt is the question) and native radios as full-width rows (`--line` border between rows, 44 px minimum height on touch; label text in `--font-ui` 16 px). No row is preselected.
- **Confidence:** a `fieldset` of five native radios, labels "Guessing", "Unsure", "Even odds", "Fairly sure", "Certain", sending `confidence` 10 / 30 / 50 / 70 / 90.
  - **No default.**
  - On desktop the five options sit in one row, each option an equal-width segment with `--line` borders and the checked one filled `--select`. On phone they stack vertically in five 44 px rows.
  - Choosing one is **required** unless "I don't know" is used. A missing value shows the status "How sure are you? Pick one." and focuses the first radio.
  - Confidence is now required where today it defaulted to 50 (P5).
- **"I don't know — show me"** is a text button that **submits immediately** with `{ answer: <typed text or "">, thinking, dontKnow: true }` and no `confidence`. Today it is a toggle plus Submit; one action is fewer steps, and the copy above the field invites a guess first.
- **Submit:** Ctrl/Cmd+Enter from any field in the block also submits.
  - While sending: controls disabled, status "Sending…".
  - On success: status "Sent".
  - On rejection: controls are re-enabled and the status shows the message in `--danger`.
  - Validation for an empty answer when not using "I don't know": "Write an answer, or choose “I don't know”."
- **`thinkingWhileReading`:** optional in the contract and not collected. Do not add a field.
- **Never put `args.answer` in the DOM** before `item.answer` exists. Show `args.explanation` only after a verdict.

Answered state (fill removed; becomes part of the page):

```
 PREDICT FIRST
 Before we compute: what do you predict …        ← prompt stays, --font-read
 │ YOU
 │ It approaches 2.                              ← learner style; reasoning below in --ink-3 if given
 │ Reasoning: slope of tangent is 2x
 │ Fairly sure (≈70%)
 ✓ Correct. You were Fairly sure.                ← verdict line, --font-ui 600 --ink
 The slope of the tangent at x = 1 is 2 …        ← verdict note + explanation, --font-read
```

- Verdict words are unchanged ("Correct", "Partly right", "Not quite"). Each gets a leading glyph `✓` / `◐` / `✗` (`aria-hidden`), always in `--ink`. No green/red, no pill.
- The verdict sentence appends the confidence: "You were ⟨label⟩." With `dontKnow`, it reads "You said you didn't know."
- **Before a free-recall verdict arrives:** the line "Waiting for feedback." in `--ink-3` (static). It is replaced in place when `verdict_record` arrives (`rerenderItem`, unchanged).
- **Read-only unanswered:** the prompt plus "Not answered." in `--ink-3`.
- **Failed tool call** (`item.failed`): not rendered (unchanged).

#### 7.2.5 Choice block (`choice_ask`)

- Same container as a pending question. Kicker: "Your choice".
- The question renders in `--font-read`.
- Options are a `fieldset` of radios (rows as above). `recommended` adds the suffix "(suggested)" in `--ink-3` after that option's label. It is **never preselected** [Johnson & Goldstein 2003].
- The last row is "Other" with a text input. Typing selects it (unchanged behavior). The input has a visible label "Other" via the row, and the `.sr-only` label "Other answer".
- The submit button reads "Continue". Ctrl/Cmd+Enter submits. Validation messages are unchanged.
- **Answered:** the question, then the learner style "│ YOU / ⟨choice⟩". Read-only unanswered: "Not answered."

#### 7.2.6 Section openers (new UI-only item kind)

When a `phase` op arrives with `phase: "teach"` and a `sectionId`, append a `section` item. Phase ops
are already in the op log; this needs no contract change.

- The section title comes from the latest `plan` op payload seen in the timeline; store it as `tl.plan` in `timeline.js`. If no `plan` op has been seen (e.g. in older logs), fall back to `s.st.plan`. If neither has the section, show "New section" with no goal line.
- Index and count use `plan.sections`.
- Skip the item if the previous `section` item has the same `sectionId`.

Rendering:

```
                                                   (--s8 space above)
 SECTION 2 OF 5                                    ← small caps --fs-meta --ink-3
 The power rule                                    ← h2 styled --fs-h1, --font-read 600
 Goal: derives the power rule for n = 2, 3         ← --fs-small --ink-2; from section.exit, omit if absent
```

- `phase: "close"` → the opener "Wrapping up" with no goal line.
- Each opener is a heading (`h2`), which gives screen-reader users a structural outline for navigation.

#### 7.2.7 Plan notices and dividers

- **`notice`:** a paragraph with `--s6` margin above and below. It starts with the small-caps label "Plan update", then the stripped notice text in `--fs-small` `--ink-2`. There is no tint, accent border or icon. Example: `PLAN UPDATE  Plan revised: added a power-rule section`.
- **Compaction divider:** see §6.4.

#### 7.2.8 Background jobs (research and plan)

One line per `jobId`, updated in place (unchanged item model). `--font-ui`, `--fs-small`, `--ink-3`.
No spinner and no colored dot.

| Kind / state | Line |
|---|---|
| research `queued` / `running` | `Research — ⟨question⟩ · running` plus a text button "Cancel" (`aria-label="Cancel research: ⟨question⟩"`) |
| research `done` with brief | A `details`, closed by default. Summary: `Research notes — ⟨question⟩`. Body: the brief rendered as `.prose`, at 0.9em inside the column. |
| research `done`, no brief yet | `Research — ⟨question⟩ · done` |
| research `failed` | `Research failed — ⟨question⟩: ⟨error⟩` (error text in `--danger`) |
| research `aborted` | `Research cancelled — ⟨question⟩` |
| plan `queued` / `running` | `Planner — drafting` plus "Cancel" |
| plan `done` | `Planner — plan ready` |
| plan `failed` | `Planner — failed: ⟨error⟩` |
| plan `aborted` | `Planner — cancelled` |

- **Job updates never move focus, never scroll and are never announced**, with one exception: a planner failure is announced (§9.6).
- A research job finishing while the learner reads produces only the in-place text change. The brief mainly feeds the tutor, and the learner can open it at will.
- A planner `turnEnd` with `error` (today a toast) becomes:
  - a notice line in the flow, `Planning failed: ⟨error⟩` (in `--danger`, `role="status"`);
  - the head label "Plan · failed";
  - the desk note "Planning failed. You can describe what you want and try again." with the composer shown, if the backend accepts messages in that state, or the note alone if it does not.

#### 7.2.9 Plan card (await approval) and the plan sheet (reference while teaching)

**Plan card** (`#plan-slot`, after the timeline, phase `awaitApproval`). Rendered as a document,
not a boxed card: a top `--hair` and `--s7` spacing.

```
 PROPOSED PLAN                                     ← small caps label
 Understand derivatives                            ← h2, --fs-h1, --font-read (the goal)
 Start from slopes of secants, then take the limit. ← approach, .prose
 Practise on polynomials before the chain rule.

 1  From secants to tangents                        ← section title, --font-read 600 --fs-read
    Guided · Socratic, short steps                  ← mode · style, --fs-small --ink-3
    Goal: can compute a derivative from the definition
    Covers: Secant slope (concept), Limit of difference quotient (concept)
    Tools  [x] graph   [x] echo                     ← native checkboxes, checked by default
 2  Power rule
    Practice · Worked examples then problems
    Goal: derives the power rule for n = 2, 3
    Covers: Power rule (skill)
    Desktop only                                    ← if section.desktopOnly, plain text

 [Approve plan]   Suggest a change                   ← primary + text button
```

- **Tools:** each section's `plugins` render as a `fieldset` (legend "Tools for ⟨section title⟩", visually shown as "Tools") of checkboxes, all checked initially. The unchecked ones form `removedPlugins` (today's `removed` set, reset per `planVersion`). This replaces the "×" chips and their "Undo" relabelling.
- **Suggest a change:** a text button that reveals (no animation) a labelled textarea "What should change, and why?" with the secondary button "Send to planner" and a "Cancel" text button. Today the textarea is always visible; this progressive disclosure keeps one primary action. The draft is kept (`planDraft`). After a successful send: status "Sent. The planner is revising the plan."; the textarea clears.
- **Approve:** "Approving…" → the card is removed when the phase changes (unchanged). The "Plan approved …" notice arrives via the op log.
- **New `planVersion`:** re-render in place, reset the tool checkboxes, and if focus was inside the old card, focus the new heading.
- **Learning-system controls (not built):** the "⋯" menus, known sections and the planner's question on the card are specified in §7.7.1–§7.7.3. Learning outcomes decide those features (`docs/learning-system.md`); this document only decides how they look.

**Plan sheet** (teach, close or ended; opened by the head's "Plan" button). A modal `<dialog>`:

- **Desktop/tablet:** anchored to the right, `inline-size: min(420px, 92vw)`, full height, `--raised` background, `--line` start border, scrim `--scrim`.
- **Phone:** a bottom sheet, full width, `max-block-size: 85dvh`, top corners `--r`.

Contents:

- Heading "Plan" plus a "Close" text button (Esc also closes).
- The goal as a paragraph.
- An ordered section list. Each section shows its title and status in words: "Done" (`✓` glyph), "Current" (`•` glyph plus `aria-current="step"`, `--ink` 600), or no marker for upcoming sections. Each section lists its nodes; a node with a correct check shows `✓` and has `.sr-only` "(checked)". The last node (`lastNodeId`) is shown in 600 weight with `aria-current="true"`. No tinted highlight rows.
- "Tools in use: graph, echo" (`activePlugins`).
- "Change plan" (secondary). It reveals the same change form; on submit it calls `request_plan_change`. The form is hidden when the session has ended.

Behavior:

- It is **closed by default at every width**. Today it is open by default on wide screens; this changes.
- The sheet re-renders only when its key changes (today's `panelKey` logic).
- Opening focuses the sheet heading. Closing returns focus to the "Plan" button (unchanged).
- The plan sheet carries the same "⋯" controls as the plan card (§7.7.1); edits made there go through the background planner job.

#### 7.2.10 Phase and progress indicator

See §6.2. In addition:

- Section openers (§7.2.6) make section changes visible in the flow.
- There is no progress bar or percentage (§2.7).
- On `ended`, the head shows "Close". The flow shows the close opener, "Wrapping up", which appears when the `close` phase op arrives.

#### 7.2.11 Errors

| Error | Where | Look | Behavior |
|---|---|---|---|
| Tutor turn failed (`turnEnd.error`) | End of the flow (`#live`) | A block with `border-inline-start: 2px solid var(--danger)`, `padding-inline-start: --s4`. Title "The reply failed." in `--font-ui` 600 `--danger`, then the message in `--ink-2`, then a secondary "Retry" button (`resume_turn`) and, when the message mentions model, connect or settings, the text button "Settings". | `role="alert"`. No focus move. Scroll only if the learner was at the end (same rule as turn start). Cleared on retry, on a new turn or on a new learner message (unchanged). |
| Unanswered last learner message (recovery) | Same block | Title "The tutor hasn't answered this yet." and the button "Get a reply". | Unchanged trigger (`offerReplyIfUnanswered`). |
| Rejected learner message | Under the desk textarea | `--danger` text, `--fs-small` | §6.3 |
| Exercise or plan command rejected | The status text inside that block | `--danger` text | Controls re-enabled |
| `session_state` failed | The head phase label becomes "Status unavailable", plus a `.sr-only` status with the message | `--ink-3` | Retried on the next state-affecting op (unchanged debounce) |
| `load_session` failed | Home: the inline status under the clicked row, "Couldn't open this session: ⟨message⟩" | `--danger` | — |
| Planner failed | §7.2.8 | — | — |

#### 7.2.12 Read-only legacy sessions

- The head shows back, title, and "Read-only" in `--ink-3` in place of the phase. There is no Plan button.
- The flow renders from the ops, with unanswered exercises showing "Not answered.".
- The desk is in the read-only state (§6.3).
- Focus on open goes to "Start a new session" with `preventScroll`.

### 7.3 Practice session

Behavior is **unchanged**:

- `send_message({ sessionId, mode: "practice", text, onEvent })` with a per-call channel;
- `retry_reply` for Retry and "Get a reply";
- `session` events update the list and title;
- Practice never calls Learn commands.

Visual changes only:

- The head: back, title, "Practice".
- The page: tutor prose and learner entries as in §6.4. Exercises are whatever the tutor writes in prose.
- The desk: composer, or composer + Stop.
- The streaming scroll rule is identical to Learn (§7.2.2).
- **Reasoning** (`thinking`): rendered *after* the reply text as a closed `details` with summary "Tutor's working". It is never auto-opened, and while streaming the summary does not change text. Today it sits above the reply.
- Errors: the block from §7.2.11 with "Retry" → `runPractice(null)`.

### 7.4 Session list and navigation

- The session list exists only on Home (§7.1).
- From a session: "← Sessions" goes Home. Focus lands on the open button of the session just left, or on the input if it was deleted.
- Delete is available from Home only.
- The open session is not marked "active" in a list, because no list is visible in sessions.
- Practice-busy navigation guard: unchanged rule, with an inline message instead of a toast (§6.2).

### 7.5 Settings

Settings is a modal `<dialog>` that fills the viewport (`inline-size: 100vw; block-size: 100dvh`), with a 640 px content column. It is opened from Home's "Settings" or from an error's "Settings" button. Scrim, no shadow.

```
 Settings                                                     Close
 ─────────────────────────────────────────────────────────────
 Model roles
 Pick a model per job. Only Tutor is required; unset roles use the Tutor model.

 Tutor (required) · heavy · teaches you        ← fieldset legend: label · weight · phrase
 Guidance text …                               ← --fs-small --ink-3
 Usage: in 12,400 tok · out 3,100 tok          ← --fs-meta --ink-3
 Provider [ WorkBuddy        ▾]
 Model    [ gpt-…            ]   (datalist)
 Reasoning effort [ medium ▾]                  ← only when the model lists efforts
 Note: Tool calling unverified.                ← warnings as plain lines, prefix "Note:"
 [Use this model]  Clear  Test   Saved — Tutor uses gpt-x   ← status inline, role=status

 ▸ Other roles (optional)                       ← details; each role as above, legend adds "uses the Tutor model" when unset

 Providers
 Keys and sign-ins stay on this device, outside your synced data.
 WorkBuddy        Signed in as …            Manage
 OpenAI           Not connected             Connect
 …

 Appearance
 Theme  ( ) System  ( ) Light  ( ) Dark
```

- Sections are `h2` headings in `--fs-ui` 600, separated by `--hair` with `--s7` spacing.
- Role "cards" become `fieldset`s with no fill. The "required" and "using Tutor model" chips become legend text.
- Statuses that are toasts today ("⟨Role⟩: using ⟨model⟩", errors) appear inline after the button row (`role="status"`, `--danger` for errors) and persist until the next action in that fieldset.
- "Test" results: "Tool call OK" (`--ink`) or the error (`--danger`).
- Provider rows: name, status text (no dot), and a secondary button "Connect", "Manage" or "Close" with `aria-expanded`. The expanded body is inline (keys, base URL `details`, WorkBuddy sign-in). The WorkBuddy waiting state shows "Waiting for sign-in in your browser…" as static text; the spinner is removed. All existing behavior (autoPick, reload, login polling) is unchanged.
- **Appearance** is new and UI-only: a radio group bound to `localStorage["lh.theme"]`.
- Phone: the same single column; button rows wrap.

### 7.6 Learning map (its own page)

The architecture's knowledge graph (proposition nodes, topic tree, FSRS state) is not built yet
(`docs/agent-system-plan.md`, "Not built yet" 1). What exists is each Learn session's approved plan:
a dependency map of ideas (`plan.nodes[].requires`) plus the check verdicts recorded against them.
The Map page draws that, and lets the learner pick ideas to practise. When the graph lands, the
data source changes (topics instead of plans); the page shape stays.

**Where it lives, and why there.** A separate page reached only from Home ("Map" in the top bar).
It is never Home's background (LearnHarness draws its graph behind the start input) and never
inside a session. A drawing in the periphery of the start question or the lesson is visual clutter
and a seductive detail (§2.1, §2.3; [Rosenholtz 2007; Sundararajan & Adesope 2020]); choosing what
to practise is a deliberate, separate task, which is when an open learner model helps most
[Mitrovic & Martin 2007; Long & Aleven 2017].

```
 ← Home   Learning map                                            (running head)
 ──────────────────────────────────────────────────────────────
 Ideas from the plans of your Learn sessions. What an idea needs
 sits above it. Pick ideas to practise them together.              (--fs-small --ink-2)
 Select: Needs work (2) · Checked (9)                              (text buttons)

 ▾ Understand derivatives                       (summary: goal, --font-read 600 --fs-read)
   From "Derivatives" · 3 Nov · 7 ideas, 3 checked, 1 needs work   (--fs-small --ink-3)
   [ ] Select all 7
        ┌──────────────┐   ┌──────────────┐
        │☐ Secant slope ✓│   │☐ Limits       │                    (solid border: checked)
        └──────┬───────┘   └──────┬───────┘
               └───────┬──────────┘
             ┌─────────┴──────────┐
             │☐ Difference quotient ◐│                            (◐ needs work)
             └─────────┬──────────┘
             ┌ ─ ─ ─ ─ ┴ ─ ─ ─ ─ ┐
             │☐ Power rule        │                              (dashed: not checked yet)
             └ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┘
   Power rule · mechanism · not checked yet · Needs: Difference quotient   (detail line)
 ▸ Chain rule drills …                                             (older plans start closed)
 ──────────────────────────────────────────────────────────────
 3 selected  Clear                                   [Practise 3 ideas]   (desk)
```

**Layout** (`ui/map-layout.js`, pure, deterministic; no simulation, no stored positions):

- One drawing per plan. Prerequisites above, dependents below: a layered DAG, which keeps
  edge crossings (the largest readability cost) low and is the layout LearnHarness itself uses for
  plans [Purchase 1997; Sugiyama 1981; Gansner 1993]. Top-down, not radial [Burch 2011].
- Layer = longest path from an idea with no in-plan prerequisite; such an idea then moves down to
  sit one layer above the first idea that needs it, so foundations stay next to their use and long
  edges do not cut across other boxes. A cycle's back edge is ignored.
  Order within a layer: plan order, then four barycenter sweeps (down, up, down, up). Layers are
  centred on the widest one.
- Boxes have a fixed size (144×56 px, 12 px apart, 40 px between layers; four fit the desktop
  column), so the layout needs no measuring and no layout reads. Titles clamp at two lines; the
  full title is in the detail line.
- The same plan always gives the same picture (mental-map stability [Misue 1995]). Nothing moves
  after render; a drawing wider than the column scrolls horizontally inside its own box.
- Edges: `--rule-strong` hairlines from the bottom of a prerequisite to the top of the idea that
  needs it, with a small arrowhead. SVG, `aria-hidden`.

**Learner state** in ink, glyph and words, never hue (WCAG 1.4.1; at most three states
[Haroz & Whitney 2012]):

| State | Rule | Box |
|---|---|---|
| Checked | at least one `correct` check verdict | solid `--line` border, `✓` after the title, `--ink` |
| Needs work | checks recorded, none `correct` | solid border, `◐` after the title, `--ink` |
| Not checked yet | no check verdict | dashed `--rule-strong` border, `--ink-2` |
| *Selected* (any state) | the box's checkbox is checked | `--select` fill and a 2 px `--accent` border |

No percentages, meters or "due" labels: FSRS state does not exist yet, and §2.7 applies.

**Controls and accessibility.** Every box is a native `<label>` holding a checkbox, so the drawing
*is* the list: Tab walks the ideas layer by layer, Space toggles, and the label carries the title
and the state in words. A hidden description gives "Needs: … · Leads to: …". Focusing or clicking
an idea writes its detail line under the drawing (kind, state, section, needs, leads to); it is
static otherwise and never announced. "Select all N" is a checkbox with the indeterminate state
for a partial selection.

**Agenda shortcuts, not preselection.** "Select: Needs work (n) · Checked (n)" add those ideas to
the selection. Nothing is selected on open (defaults steer choices [Johnson & Goldstein 2003]);
the shortcuts point at what is worth practising (missed checks first), since unaided learners tend
to pick easy or massed material [Kornell & Bjork 2007; Kornell & Metcalfe 2006].

**Desk.** One state: "n selected", a "Clear" text button, and the one primary button,
"Practise n ideas" (disabled, reading "Practise", with nothing selected). Pressing it starts a
Practice session whose first message lists the selected ideas and asks for them mixed, not one at
a time [Rohrer & Taylor 2007; Brunmair & Richter 2019]. Its first line doubles as the session
title until the generated title arrives:

```
Practise: Power rule, Chain rule

Mix them together rather than one at a time. From my Learn sessions:
- Power rule (from “Understand derivatives”)
- Chain rule (from “Understand derivatives”)
```

Without a model the desk shows "Choose a model first." in `--danger` under the button and opens
Settings. The selection survives leaving the Map for Home and coming back; it is cleared once a
practice session starts.

**Groups.** One `details` per plan, newest first. The newest is open; older ones start closed.
The summary is the plan goal; the line under it gives the session title, date and counts in words.

**States.** Loading: "Loading the map…" (static). Empty: "Nothing to map yet. Ideas appear here
once you approve a plan in a Learn session." Failure: "Couldn't load the map: ⟨message⟩" in
`--danger` with a "Try again" text button. The page is loaded once per visit and does not update
while open (P3).

**Focus.** Opening focuses the page heading (`tabindex="-1"`); "← Home" returns focus to Home's
"Map" button.

**Backend.** One read-only command, `learning_map()`, returns
`[{ sessionId, title, updatedAt, goal, nodes: [{ id, title, kind, requires, section, correct, missed }] }]`
for every Learn session that is not read-only and whose plan was approved at least once, newest
first. `correct` counts `correct` check verdicts for the node; `missed` counts the others. It reads
the op logs without disturbing the open-session cache.

**Performance.** One IPC call per visit. No layout reads while rendering. Each group has
`content-visibility: auto`, so closed and off-screen plans cost nothing to lay out.

**Learning-system additions (not built; §7.7.6):** the reached and aimed level in the detail line, a "Where the field lives" disclosure and a calibration line per plan group.

### 7.7 Learning-system surfaces (not built)

These surfaces present the features in `docs/learning-system.md`. That document says *what* and *why*; this section only says how they look, under the Carrel rules:
- one primary action;
- no badges or pills;
- native controls;
- nothing moves on its own;
- extra controls hidden until asked for.

Where these rules and a learning feature conflict, the feature wins and this section changes.

#### 7.7.1 Plan card "⋯" controls

```
 PROPOSED PLAN                                              ⋯   ← text button "Plan options", aria-haspopup="dialog"
 Understand voltage dividers
 You already use Ohm's law fluently …

 1  Series current                                          ⋯   ← "Options for ⟨section title⟩"
    Guided · Socratic, short steps
    Goal: explains why the current is the same everywhere in one path
 2  Divider rule
    Guided · Socratic · Aim: Use                                ← "Aim: ⟨level⟩" only with levels
    Goal: finds an unknown resistor from a measured output
    Analyze only · Hunt on                                      ← summary of THIS learner's edits, --fs-small --ink-2
 3  Loaded dividers                                         ⋯
    …

 [Send changes]   Discard                                      ← replaces Approve while edits are pending
```

- **"⋯" opens an inline disclosure** under its row, never a popover over other content. Its controls are native:
  - **Plan options:**
    - Depth (radios: Just pass / Solid / Deep);
    - Explore (radios: Off / Suggest / On);
    - Sitting length (number input, minutes);
    - Style (radios: "Let me reason it out" / "Explain it to me").
  - **Section options:**
    - How far (radios: Explain / Use / Analyze / Design, each with a one-line description);
    - Include (radios: Keep / Don't need this);
    - "Parts I already know…" (textarea);
    - Tools (checkboxes; moved here from the always-visible fieldset);
    - Hunt (checkbox), present only when the planner marked a hunt available, with the planner's one-line reason as its description;
    - Note for the planner (textarea).
- **Edits are local until sent.** While any edit is pending:
  - the primary button reads **"Send changes"**, with an optional "Comment for the planner" textarea revealed above it;
  - "Discard" is a text button;
  - **Approve is not shown.**
  - Sending writes one `edit_plan` call; the status reads "Sent. The planner is revising the plan."
- **Edited sections** show a one-line summary of the learner's own choices ("Analyze only", "Don't need this", "You know part of this", "Hunt on") in `--fs-small --ink-2`. Defaults aren't summarized.
- A planner **warning** about a choice (e.g. a level below the goal) appears as one sentence in the approach text, not as an alert.

#### 7.7.2 Known sections

- A section with `assumedKnown` renders in `--ink-3` with the line "You said you know this. One quick check when we get here." The title stays readable, and nothing is struck through.
- In the plan sheet: "Known · not checked yet" → "Known · checked ✓" after a pass. After a miss (`knownRefuted`), the section renders normally and its line reads "Checked: needs teaching after all".
- The section opener in the flow (§7.2.6) for a known section reads "SECTION 2 OF 5 · QUICK CHECK".

#### 7.7.3 Planner question on the card (`plan_ask`)

- Rendered inside the plan card, above the sections, with the same markup as a choice block (§7.2.5).
- Kicker "The planner asks"; the submit button reads "Answer".
- While it's pending, "Send changes" and Approve are hidden: the question is the one action.
- When answered, it collapses to "You answered: ⟨choice⟩" until the new plan version replaces the card.

#### 7.7.4 Hunt card

A new flow item (kind `hunt`), with the same container as a pending question (`--raised`, `--line`, `--r`).

```
 HUNT                                                    ← small caps, --fs-meta --ink-3
 Find out why op-amp circuits almost always use negative
 feedback, and where people disagree about it.          ← the question, --font-read --fs-read

 Where to look
 • A forum thread on r/AskElectronics: people explain it to beginners
 • TI's "Op Amps for Everyone", chapter 3: the reference treatment
 Bring back: what you found (with sources if you have them),
 your questions, anything surprising, practical tips.
 No deadline. Come back whenever you're ready.

 What you found
 [ notes …                                         ]      ← textarea, grows
 Links  [ https://… ]  + Add link                         ← one input per link
 Files  [Attach…]                                         ← native file input, multiple
 [I'm back]    Skip hunt                                  ← primary + text button
```

- Nothing in the return form is required. "I'm back" with an empty form asks once, inline: "Send with nothing? You can also skip." (Send / Skip).
- **While the hunt is out,** the desk stays usable (the learner can ask the tutor things), and the running head shows "Hunt" next to the section label. Nothing reminds or nags; there's no timer.
- "Skip hunt" is visible before and during the hunt; it needs no confirmation.
- **Answered state:** the question, then "│ YOU" with the notes, links and file names. Files the app can't read yet show "(not readable yet; the tutor has your description)".
- **The debrief** follows as an ordinary tutor reply. The tutor's sorted claims (`debrief_record`) render after it as a compact list: each claim with its verdict word and glyph ("Correct ✓", "Partly ◐", "Not right ✗", "Rule of thumb", "Saved for later"), in ink, not colour.

#### 7.7.5 Close: field guide and calibration

After the close text, two disclosures (closed by default; these are reference material, not reading):
- **"Where the field lives"**:
  - "Useful now", grouped as Ask / Discussion / Reference;
  - "Later", with what unlocks each venue ("after op-amps").

  Each venue is a name, a link, and its one-line note. With depth `pass`, only Ask.
- **"Your confidence"**: one line per reported confidence choice ("Fairly sure: right 9 of 15"). Shown only when the calibration threshold is met.

Frontier items parked in the session appear as one line, "Saved for later: ⟨topics⟩", with a text button "Start a session" per item.

#### 7.7.6 Map page additions

- **Detail line:** adds "Reached: ⟨level⟩ · Aim: ⟨level⟩" when known. Box states stay the three of §7.6; the level is words, not a fourth visual state.
- **Per plan group:**
  - a closed "Where the field lives" disclosure (venues from that session's brief; moves to topics when the graph exists);
  - one calibration line under the counts, when the threshold is met.
- **Frontier:** a "Saved for later" list at the bottom of the page. Each item has a "Start a session" text button.

#### 7.7.7 Settings

- **Learning** section (new, between Model roles and Providers): "Starting point for new sessions", with the radios Measure me / Quick check / I'm new to this. The description reads "How much I check what you already know before planning. You can change it per session."

---

## 8. Motion policy

**May move (exhaustive list):**

1. **Sheet and dialog open** (plan sheet, Settings): opacity 0→1 plus `translate` 8 px→0 (from the right on desktop sheets, from below on phone sheets) over `--dur-sheet-in` with `--ease-out`. Close: opacity 1→0 over `--dur-sheet-out` with `--ease-in`. The backdrop fades with the same timing.
2. **Hover and pressed color changes** on controls: `background-color`, `color`, `border-color` over `--dur-hover`, linear. These are foveal and user-caused.

**Must never animate:**

- streaming text, blocks or the tail;
- any caret;
- "Writing…";
- job state changes;
- the phase label;
- section openers;
- verdict appearance;
- new timeline items;
- the desk switching state;
- disclosure open/close (`details`);
- layout or size;
- scroll position (all programmatic scrolls use `behavior: "instant"`, and `scroll-behavior` stays `auto`);
- focus rings;
- any looping or `infinite` animation (spinners, pulses, blinks, shimmer skeletons).

**`prefers-reduced-motion: reduce`:** set both sheet durations to `0ms` and disable the translate. Hover transitions may remain, since they are color only. Implement as one block that zeroes `--dur-*`.

**Enforcement** (checked in §12):

- `styles.css` has no `@keyframes` except, optionally, the sheet keyframes. Prefer `transition`.
- It has no `animation:` declarations other than those.
- At runtime, `document.getAnimations()` is empty except during a sheet transition.

---

## 9. Interaction rules

### 9.1 Focus management

General rule: programmatic focus always uses `{ preventScroll: true }`. Focus moves only to replace
focus that would otherwise be lost (its element was hidden or removed) or to land on the
learner's next required action when they are not engaged elsewhere. "Not engaged elsewhere" means
`document.activeElement` is `body`, the desk textarea or a control that has just been
disabled or removed.

| Event | Focus goes to | Scroll |
|---|---|---|
| Home loads | The Home input | None |
| Session started from Home | Desk textarea | Top of flow |
| Existing session opened | (1) the pending exercise's first field, (2) the plan card heading if awaiting approval, (3) "Start a new session" if read-only, (4) the desk textarea otherwise | Align the last tutor entry's top under the head. If a pending exercise or plan card does not fit below it, align that block instead (resumption cue). |
| Learner sends from the desk | Stays in the textarea | Make the new "You" entry visible (instant, minimal) |
| Tutor turn starts | Unchanged | Once, by the §7.2.2 rule |
| Text deltas / finalise | Unchanged | None |
| Exercise (question or choice) arrives pending | The first field (answer textarea, or first radio), only if not engaged elsewhere; the composer is being hidden, so its focus would be lost anyway | **None** |
| Exercise submitted successfully | The block's status line ("Sent", `tabindex="-1"`) | None |
| Verdict arrives | Unchanged | None |
| Plan card appears (awaitApproval) | The plan heading (`tabindex="-1"`), only if not engaged elsewhere | None |
| Plan re-rendered (new version) | The new heading, only if focus was inside the old card | None |
| Plan approved, composer returns | The desk textarea, only if not engaged elsewhere | None |
| "Go to question" / "Go to plan" in the desk | The first field or heading of the target | `scrollIntoView({block:"center", behavior:"instant"})` |
| "↓ Continue reading" | The desk textarea (or the desk note's button), because the activated button hides itself | Instant, as in §7.2.2 |
| Plan sheet opens / closes | The sheet heading / the "Plan" button | n/a |
| Settings opens / closes | The first control / the opener | n/a |
| Turn error | Unchanged | Only if the learner was at the end |
| Delete session (Home) | The next row's open button, else the previous row's, else the Home input | None |
| Back to Home | The open button of the session just left | Restore the list position |

### 9.2 Keyboard

The set is kept deliberately small, with no single-letter global shortcuts (they conflict with screen readers and typing).

| Keys | Where | Action |
|---|---|---|
| Enter | Desk textarea / Home input | Send / Start (unchanged; not while busy; respects IME `isComposing`) |
| Shift+Enter | Same | New line |
| Ctrl/Cmd+Enter | Any field in an exercise block or a change-plan form | Submit that form |
| Esc | Plan sheet, Settings | Close (native `dialog` cancel) and restore focus |
| Tab / Shift+Tab | Everywhere | Order: head (back → Plan) → page content in reading order (links, research disclosures, exercise fields) → desk |
| Arrow keys | Radio groups (mode, options, confidence, theme) | Native radio behavior |

Not added: Esc to stop a reply (too easy to trigger by accident; Stop is one Tab away in the desk).

### 9.3 Background events while the learner reads

| Event | Visible effect | Announced? |
|---|---|---|
| Research job queued, running, done or aborted | Job line text changes in place | No |
| Research failed | Job line shows the error | No (it is not blocking) |
| Planner running | Head label "Planning…", job line, desk note (when in the plan phase) | Head label has `role="status"` → once |
| Plan ready for approval | Plan card at the end of the flow, desk note "Review the plan to continue." | "Plan ready for review." |
| Planner failed | Notice line, head label, desk note (§7.2.8) | "Planning failed." |
| Plan revised during teach | Notice line at the end of the flow; head label updates silently | No |
| Title changed | Head title text | No |
| Events for other sessions | Home list entry only (if Home is shown later) | No |

Nothing in this table scrolls, moves focus or animates.

### 9.4 Composer

- One instance (`#composer`), moved between the Home slot and the desk (unchanged mechanism).
- Draft survives being hidden (pending exercise, approval). It is cleared only on a successful send.
- Auto-grow up to `max(30dvh, 160px)`, then the textarea scrolls internally.
- While busy, the textarea is editable; Send becomes Stop (unchanged logic in `syncBusy`). The visible label is "Stop", not just an icon.
- Placeholders: "Reply to the tutor" in sessions; the `COPY` placeholders on Home. Remove `updateComposerHint`'s "Answer the question above first…" and "Approve the plan above…", because the desk note replaces them.

### 9.5 Scrolling

- **No follow mode.** Remove `state.stick`-driven scrolling from `LiveReply.onPaint`, `renderPlanCard`, `showError` and `onOp`. Keep `nearBottom()` only as the "at the end" test for the turn-start rule.
- The lazy timeline is unchanged: `PAGE = 40` items mounted, `prependOlder` on scroll near the top, `fillViewport` for short histories, and manual anchor compensation.
- `content-visibility: auto` stays on `.timeline > *`. Raise `contain-intrinsic-block-size` from `auto 140px` to `auto 240px` to match the larger type; `auto` still remembers real sizes.

### 9.6 Screen-reader announcements

- `#timeline` keeps `role="log"` but set `aria-live="off"`. Announcing a whole 20 k-character finished reply is hostile.
- Add one visually hidden polite announcer: `<div id="announcer" class="sr-only" role="status" aria-live="polite">`. Write short messages to it (clear it first, then set the text on the next frame):
  - "Reply finished."
  - "Question: ⟨first 140 characters of the prompt, Markdown stripped⟩"
  - "Choice: ⟨first 140 characters⟩"
  - "Feedback: Correct." / "Feedback: Partly right." / "Feedback: Not quite."
  - "Plan ready for review."
  - "Planning failed."
  - "New section: ⟨title⟩"
- Turn errors use `role="alert"` on the error block (§7.2.11). No other alerts.
- `prefers-reduced-motion` and `forced-colors` are honored (§5.3, §8).

### 9.7 Errors: general rules

- Inline at the source, persistent until the condition changes, with the action that fixes it next to the message (Retry, Settings, Try again).
- `--danger` is used only for system failures, never for the learner's wrong answers.
- Remove `#toast` and `toast()`. Every caller gets an inline target: Home, the desk, the exercise status, the Settings fieldset status, or the head status (practice guard).

---

## 10. "Classic AI look" removal map

| # | Current pattern (file) | Replacement |
|---|---|---|
| 1 | Persistent history sidebar with "New session" button, gradient brand mark, Settings footer (`index.html` `.sidebar`, `styles.css`) | Home screen list (§7.1) and "← Sessions" in the head (§6.2) |
| 2 | Gradient logo and avatar (`linear-gradient(135deg, var(--accent), #b07cff)`; `reply.js` `BRAND_SVG` avatar) | Plain text wordmark on Home; no avatar in the flow |
| 3 | "Tutor · model" meta row on every reply (`reply.js` `shell`/`setMeta`) | No meta. Endings as a closing italic line (§6.4). The model is visible only in Settings |
| 4 | Right-aligned user bubbles (`.msg.user .bubble`) | Left-aligned "You" notes with an inline-start rule (§6.4) |
| 5 | Floating rounded composer with shadow, focus-colored border, "Enter to send…" hint and circular arrow button | Flat desk with a hairline, labelled textarea, text button "Send" / "Stop" (§6.3) |
| 6 | Model pill with glowing status dot (`#model-pill`, `.dot` box-shadow glow) | Removed from sessions; model status lives in Settings and the Home setup notice |
| 7 | "LEARN" uppercase pill, colored mode dots in the list | The word "Practice" / "Read-only" in the head; "Learn"/"Practice" as list meta text |
| 8 | Uppercase letterspaced accent kickers ("QUICK QUESTION", "YOUR CALL") | Small caps in `--ink-3` (§7.2.4) |
| 9 | Boxed cards with header bars, borders and shadows for everything | Typographic blocks; only the *pending* exercise has a fill and border |
| 10 | Pills and badges ("Correct", "Answered", "guided", "Done", "Desktop only"), plugin chips with "×" | Verdict words with glyphs, plain text, checkboxes (§7.2.4, §7.2.9) |
| 11 | Indigo/violet accent with purple gradient | One lamp-green accent, used sparingly (§5.3) |
| 12 | Bouncing typing dots, blinking caret, spinners | Static "Writing…", no caret, static job text (§6.4, §7.2.8) |
| 13 | Stream-following autoscroll | Turn-start alignment once, then no movement (§7.2.2) |
| 14 | Suggestion chips | Removed; Home has no suggestions (§7.1) |
| 15 | Icon segmented control as `role="tablist"` | Radio group with descriptions (§7.1) |
| 16 | Toasts | Inline persistent status (§9.7) |
| 17 | "Reasoning" disclosure above every reply | Hidden in Learn; closed "Tutor's working" after the reply in Practice (§7.2.2, §7.3) |
| 18 | Two header bands (topbar plus phase strip) | One running head (§6.2) |
| 19 | Bold centered sans hero | Serif display line, left-aligned in the column (§7.1) |
| 20 | Plan side panel open by default | Plan sheet on demand (§7.2.9) |
| 21 | Confidence slider defaulting to 50 % | Five labelled radios, no default (§7.2.4) |

---

## 11. Constraints the implementer must keep

1. **Backend contract, exactly.** Use only the commands and events listed below, with the payload shapes given. The one Rust addition is the read-only `learning_map` command for the Map page (§7.6); nothing else in Rust changes for the UI.
   - **Events:** subscribe once via `subscribe_events({ onEvent: channel })`. The event kinds are `session`, `turnStart`, `text`, `thinking`, `op` and `turnEnd(error?)`.
     - Ignore events for other sessions, except `session` (list upsert).
     - Live text is replaced by the assistant `message` op, which is the persisted truth.
   - **Learn commands:**
     - `start_teaching({ text })`
     - `send_learner_message({ sessionId, text })`
     - `answer({ sessionId, callId, payload })`
     - `approve({ sessionId, removedPlugins:[{sectionId, plugin}] })`
     - `request_plan_change({ sessionId, text })`
     - `session_state({ sessionId })` — debounced ~50 ms after `phase`/`plan`/`approve`/`pluginGrant`/`tool` ops
     - `cancel_reply`, `cancel_job({ sessionId, jobId })`, `resume_turn`
     - `list_sessions`, `load_session({ id })`, `delete_session({ id })`
   - **Payloads for `answer`:**
     - `question_ask`: `{ answer, thinking, confidence? (omit when dontKnow), dontKnow }`. `thinkingWhileReading` is optional and not sent.
     - `choice_ask`: `{ answer }`.
   - **Ops:**
     - Only tutor tool ops are shown; planner and other tool ops stay hidden.
     - `args.answer` is never shown before the learner answers; `args.explanation` only after a verdict.
     - A `research-brief` message is the job's done state.
     - A `plan-notice` message renders as a notice with `[plan update] ` stripped.
     - Assistant messages with empty `content` are not rendered.
     - `job` ops update one line per `jobId`.
     - `compaction` → divider; `title` → head title.
   - **Practice** keeps `send_message({ sessionId, mode:"practice", text, onEvent })` and `retry_reply` with per-call channels, exactly as today. Learn never calls `send_message`.
   - **Legacy** `readOnly` sessions: render read-only, no composer, "Start a new session" → Home.
2. **Accessibility (hard requirement):**
   - WCAG 2.2 AA at minimum: contrast as in §5.3, 1.4.11 non-text contrast for control borders and focus, 2.4.7 and 2.4.11 focus visible and not obscured (scroll-padding), 2.5.8 target size, 1.4.10 reflow at 320 px, 1.4.12 text spacing (nothing clips if line-height becomes 1.5× and paragraph spacing 2×), 2.2.2 no blinking, 4.1.3 status messages.
   - Real labelled controls only: native `button`, `input`, `textarea`, `fieldset`/`legend`, `dialog`, `details`.
   - Keyboard-only operation of every flow.
3. **Performance mechanisms stay intact:**
   - `LiveReply`'s block cache (`createSplitter`, completed blocks inserted once, tail-only re-render, rAF-coalesced paint, `finalize` reusing DOM);
   - the KaTeX/markdown render cache;
   - the lazy timeline (`PAGE = 40`, `prependOlder`, `fillViewport`, anchor compensation);
   - `content-visibility: auto` with `contain-intrinsic-block-size: auto …` on timeline children;
   - in-place `rerenderItem`;
   - incremental session-list updates (one entry per `session` event, no re-list);
   - `session_state` debounce;
   - deferred KaTeX warm-up via `requestIdleCallback`.
   Removing autoscroll must not add layout reads per paint.
4. **No new dependencies, no bundler, no runtime font downloads.** System font stacks only (§5.1). Vendored marked, DOMPurify and KaTeX are unchanged; KaTeX's CSS is only overridden for `font-size` (§5.2).
5. **Practice behavior unchanged** (§7.3): same commands, channels, retry and recovery, auto-title. Only its rendering changes.
6. **IDs used by `app.js` stay or are renamed in the same change:** `#messages`, `#timeline`, `#plan-slot`, `#live`, `#composer`, `#prompt`, `#send`, `#home`, `#conversation`, `#plan-panel` (now a `dialog`), `#settings`. This is a clean cutover: remove `#toast`, `#sidebar`, `#scrim`, `#model-pill`, `#session-mode`, `#phase-strip`'s separate band, `.only-mobile`, and their dead CSS and JS.
7. Do not copy code or text from LearnHarness (AGPL-3.0).

---

## 11a. Implementation notes (verification, 2026-11-05)

Implemented in `ui/` as specified. Verified in headless Chromium 155 against
`window.__TAURI__` mock (`/tmp/lh-bench/mock2.js`), at 1440×900 and 390×844 in
light and dark, with no console errors. All §12 items pass; measured numbers:

| Check | Result |
|---|---|
| A3 typography | serif `"Iowan Old Style", Charter, …`, 20px/31px desktop (18px/27px phone) |
| A4 characters per line | 64.5 (canvas `measureText` average-character method) |
| A5 contrast (light) | ink 16.49:1, ink-3 5.47:1, rule 3.51:1, accent/focus 7.42:1 |
| A5 contrast (dark) | ink 14.12:1, ink-3 5.89:1, rule 3.72:1, accent/focus 8.97:1 |
| A6 CSS grep | no `linear-gradient`, `999px`, `uppercase`, `box-shadow`, `infinite` |
| F2 20 000-char stream | new: max frame after 1 s **9.2–10.2 ms**, 0 frames > 16 ms, 2.8 s total work; old full re-render: **155.2 ms**, 653 frames > 16 ms, 46.1 s work. Output identical (288 KaTeX, 12 `<pre>`). |
| F1 lazy timeline | 40 items mounted on open; 40 → 80 → 120 on scroll-to-top |
| M1/M2/M3 motion/scroll | 0 animations during streaming/jobs; `scrollTop` unchanged (600 → 600) when scrolled up; aligned once at turn start (11658 → 12339) and constant during deltas |
| M4 reduced motion | CDP `prefers-reduced-motion: reduce` → 0 running animations when the plan sheet opens |
| E1–E7 | desk guard + hidden composer; answer sentinel absent from `innerText`; confidence radios unchecked with validation; `answer` payloads `confidence: 70` / `dontKnow: true`; verdict "✓ Correct. You were Fairly sure."; "(suggested)" with no preselection; focus lands in the answer textarea |
| P1–P4 | plugin checkboxes → `removedPlugins:[{sectionId:"s1",plugin:"graph"}]`; change draft survives re-render; "Teach 1/2" head, closed plan sheet, focus returns to Plan on Esc; section opener "Section 2 of 2 / Power rule" announced |
| B1–B4 | job line updates in place with no scroll/focus/announcement change; `turnEnd` error → inline `role="alert"` + Retry → `resume_turn`; rejected send restores the draft; planner failure → notice + "Plan · failed" head label once a `phase: plan` op arrives |
| S1–S5 | legacy read-only with "Start a new session"; Practice `send_message`/`retry_reply` unchanged; Home radio group with descriptions, 59 px rows, two-press delete; theme persists across reload; phone `scrollWidth = 390` |
| K1/K2 | keyboard-only Home → question → answer → confidence → Ctrl+Enter → verdict; every `:focus-visible` tab stop gets a 2 px accent ring |

One nuance: the "Plan · failed" head label is rendered from `session_state`'s
`phase`. A planner failure emitted while the cached state still says `teach`
shows the notice line and desk note first; the head label appears when the
back-end's `phase: plan` op arrives (which is when a real planner run starts).
This is not a deviation from the contract — the UI never invents state.

### Revision, 2026-11-06

An audit of `ui/` against this document, plus the Map page (§7.6). Verified in headless
Chromium 155 against the same mock with a `learning_map` handler, at 1440×900 and 390×844, light
and dark; `learning_map` itself was checked against real op-log files in a throwaway test.

| Area | Change |
|---|---|
| Home | The "Or try:" suggestions and the visible Enter hint are gone (owner decision; §6.3, §7.1). "Map" sits next to "Settings". |
| Finalize jump | The streaming reply carried the class `live`, which also matched `.live > *` (32 px margin) until finalize removed it: the reply jumped up 32 px. It is now `.streaming`; measured shift on finalize: 0 px. |
| Mid-turn scroll | A second model call in the same turn re-ran the turn-start alignment. Now only `turnStart` aligns (§7.2.2); measured: `scrollTop` unchanged across a second segment. |
| Anchoring | Scroll anchoring was switched off for the whole scroller while streaming; it now stays on, with only the streaming entry and `#live` excluded (§7.2.2). M2/M3 re-measured: unchanged `scrollTop`. |
| Reserved space | Released below the viewport at turn end and on scroll, instead of staying until the next turn. |
| Continue reading | No longer a desk row that resized the desk and scroller on every toggle; it rests on the desk edge (§7.2.2). |
| Read-only open | An unanswered legacy question stole the resumption target and dropped focus; focus now lands on "Start a new session". |
| Midway join | "Still working…" no longer appears while a joined-mid-turn reply is actively being written. |
| Announcements | Messages in the same frame are joined instead of overwriting each other. "Reply finished." is suppressed only after a question or choice announcement in that turn. The "hasn't answered yet" recovery block is `role="status"`, not `alert`. |
| Math | Blocks streamed before KaTeX loaded are re-rendered at finalize instead of keeping raw TeX. KaTeX's stylesheet is loaded with its script (inserted before `styles.css`), so it no longer blocks Home's first paint. |
| Dead code | The orphan-stream path (unreachable: the message op always precedes `turnEnd`), `phaseLabel`, the `isBusy` alias and redundant CSS rules are removed. |
| Focus | Every programmatic focus uses `preventScroll` (validation focus in exercises, plan change and API-key forms). |
| Settings | One primary per screen: "Use this model" is primary for Tutor only; WorkBuddy sign-in is secondary. API-key fields have a visible label. |
| Other | Armed "Delete?" is ink 600, not `--danger` (not a failure). The Plan button underlines only on hover/focus. The Send/Stop swap never fades. |
| Performance | Layout reads after ops are coalesced into one rAF with the scroll handler; the pending-exercise lookup is O(1) (`tl.lastExercise`) instead of a scan per op; section openers refresh once per plan version instead of on every state refresh; the plan sheet key no longer serialises all check history; empty turn-tracking entries are pruned. |

Kept deliberately: after a planner failure with the composer hidden, the desk note reads
"Planning failed." without "You can describe what you want…", because there is no input to
describe it in.


---

## 12. Acceptance checklist (verifiable with screenshots and small scripts)

Use the mocked backend: serve `ui/` with `index.html` patched to load a `window.__TAURI__` mock first. A mock like `/tmp/lh-bench/mock.js` provides sessions `L1` (await approval), `long` (500 messages, teach), `legacy` (read-only) and `pr` (practice), plus `__mock.emit(event)`. Screenshot each item at **1440×900** and **390×844**, in **light and dark**.

**Look**

- [ ] A1 The session view shows only the running head, page column and desk. There is no sidebar, model pill, avatar, gradient, pill-shaped element or drop shadow.
- [ ] A2 At 390 px the session title is visible in the head.
- [ ] A3 Tutor prose's computed `font-family` resolves to the serif stack. Its `font-size` is 20 px (desktop) / 18 px (phone) and its line-height is 31 px / 27 px.
- [ ] A4 Characters per line, measured on a long tutor paragraph (canvas `measureText` average-character method, or count characters on the first full line), are **58–72 on desktop**.
- [ ] A5 Contrast spot-check with computed colors: body ink ≥ 7:1, `--ink-3` text ≥ 4.5:1, control borders and focus ring ≥ 3:1, in both themes (values in §5.3).
- [ ] A6 A grep of `styles.css` finds no `linear-gradient`, no `999px`, no `text-transform: uppercase`, no `box-shadow`, and no `infinite`.

**Motion and scroll**

- [ ] M1 During streaming and while a research job runs, `document.getAnimations().length === 0`.
- [ ] M2 Start a stream with `__mock.emit` while scrolled up by 600 px, and emit 30 text deltas. `#messages.scrollTop` is unchanged (±1 px), checked before and after with two screenshots.
- [ ] M3 With the learner at the end, a new turn whose start would be below the fold is aligned once under the head; `scrollTop` then stays constant for the rest of the stream.
- [ ] M4 With `emulateMediaFeatures([{name:"prefers-reduced-motion", value:"reduce"}])`, opening the plan sheet produces no running animations or transitions.
- [ ] M5 No toast element exists in the DOM; `#toast` is gone.

**Exercises**

- [ ] E1 For a pending `question_ask`, the desk shows "Answer the question to continue." and the composer is `hidden`. The block is open even for `role: "check"`.
- [ ] E2 For a pending question, `document.body.innerText` does not contain the mock `args.answer` (use a distinctive answer string).
- [ ] E3 No confidence radio is checked initially. Submitting with an answer but no confidence shows "How sure are you? Pick one." and does not call `answer`.
- [ ] E4 Choosing "Fairly sure" and submitting calls `answer` with `confidence: 70`. "I don't know — show me" calls `answer` with `dontKnow: true` and no `confidence`. Check via `__mock.calls`.
- [ ] E5 After a `verdict_record`, the block shows "✓ Correct. You were Fairly sure." and the explanation, in ink color, with no pill.
- [ ] E6 A choice card with `recommended` shows "(suggested)", and no option is preselected.
- [ ] E7 Focus lands in the answer field when the question arrives while focus was in the composer, and `scrollTop` did not change.

**Plan and progress**

- [ ] P1 Session `L1`: the plan renders as a document. Tools are checkboxes, all checked. Unchecking "graph" and approving sends `removedPlugins: [{sectionId:"s1", plugin:"graph"}]` (check `__mock.calls`).
- [ ] P2 "Suggest a change" reveals the labelled textarea; the draft survives a re-render of the same plan version.
- [ ] P3 Session `long`: the head shows "Measure · Plan · Teach 1/2 · Close" on desktop and "Teach 1/2" on phone. The plan sheet is closed on open. "Plan" opens it, Esc closes it and focus returns to "Plan".
- [ ] P4 Emitting a `phase` op `{phase:"teach", sectionId:"s2"}` appends the opener "SECTION 2 OF 2 / Power rule / Goal: …" and announces "New section: Power rule".

**Background and errors**

- [ ] B1 Emitting `job` ops for research running → done while scrolled mid-page changes only the job line text. Focus and `scrollTop` are unchanged and nothing is announced.
- [ ] B2 Emitting `turnEnd` with `error` shows the inline error block with "Retry" and `role="alert"`. "Retry" calls `resume_turn`.
- [ ] B3 A rejected `send_learner_message` (mock handler throws) puts the text back in the textarea and shows the message under it.
- [ ] B4 Emitting a planner `turnEnd` with `error` shows the notice line and the head label "Plan · failed". No toast.

**Other surfaces**

- [ ] S1 Session `legacy`: there is no composer. The desk shows the read-only note and "Start a new session", which goes to Home.
- [ ] S2 Session `pr` (Practice): sending calls `send_message` with `mode:"practice"` and a fresh channel. Retry calls `retry_reply`. The head shows "Practice", and there is no phase or Plan.
- [ ] S3 Home: the mode control is a radio group with descriptions. There is no "Or try:" line and no visible Enter hint. The list shows group headings and rows at least 48 px tall. Delete needs two presses.
- [ ] S4 Settings: role fieldsets with inline status after "Use this model". Appearance → Dark switches the theme, and the choice persists across a reload.
- [ ] S5 At 390 px nothing scrolls horizontally (`document.documentElement.scrollWidth <= 390`), and every button is at least 44 px tall under `pointer: coarse` emulation.

**Keyboard-only walk**

- [ ] K1 Using only the keyboard: Home → type a topic → Enter → (mock) the question arrives → type an answer → arrow to a confidence → Ctrl+Enter → the verdict appears → Tab reaches the desk.
- [ ] K2 A visible focus ring at every Tab stop, never hidden under the head or desk.

**Performance**

- [ ] F1 Opening `long` mounts ≤ 40 timeline items. Scrolling to the top prepends more without a visible jump.
- [ ] F2 A 20 000-character streamed reply (bench page, if present) still renders with the block cache: completed blocks are not re-rendered, which you can check by counting `renderBlock` calls or comparing with the existing bench result.

**Map (§7.6)**

- [ ] G1 Home → "Map" shows the Map page with one group per approved plan, newest open; Home and sessions show no drawing. "← Home" returns focus to "Map".
- [ ] G2 In a group, every prerequisite box sits in a higher layer than the boxes that need it; reopening the page gives identical box positions.
- [ ] G3 Boxes show ✓ / ◐ / dashed border for checked / needs work / not checked, and the label's accessible name contains the state in words. No hue other than the selection accent is used.
- [ ] G4 Nothing is selected on open. "Needs work (n)" selects exactly those n ideas; "Select all" shows the indeterminate state for a partial group; the desk count follows.
- [ ] G5 "Practise n ideas" calls `send_message` with `mode:"practice"` and a first message listing the selected titles; the session view opens in Practice.
- [ ] G6 `document.getAnimations()` is empty on the Map page, and at 390 px only a drawing's own box scrolls horizontally (`document.documentElement.scrollWidth <= 390`).

---

## 13. Limits of this evidence base

- Search worked through both the aggregator and `web_search`. Every DOI below was checked against Crossref, except Arditi & Cho 2005 and Sunny & von Mühlenen 2011, which are linked through ResearchGate and PubMed instead.
- I read **abstracts and summaries, not full texts**, for most papers (Rello 2016's PDF did not download). Specific numbers quoted from summaries (Altmann 2014's 2.8 s/4.4 s, g values, Fisher 2014's percentages) should be treated as approximate.
- No study tests *streaming AI text* or *auto-scroll during generation* directly. The no-follow rule extrapolates from the regression evidence [Schotter 2014] and from onset/motion capture.
- Dark-vs-light *comfort* and *halation* claims are mostly practitioner lore. Only the legibility findings are empirical.
- Flow evidence is correlational. Calm technology is a framework, not an experiment.
- Serif-for-register is a design choice, not a legibility claim.
- Confidence-rating reactivity [Double & Birney 2019] shows that the confidence scale may help some learners and hurt others. Its benefit here rests on hypercorrection [Butterfield & Metcalfe 2001] and on the tutor's use of the signal.
- The developer's machine only showed one serif fallback chain (Noto Serif). Rendering on Windows (Sitka/Cambria) and macOS (Iowan/Charter) was not observed.

---

## 14. Sources

Attention, motion, interruption
- Yantis & Jonides 1984 — Abrupt visual onsets and selective attention. *JEP:HPP* 10(5). https://doi.org/10.1037/0096-1523.10.5.601
- Abrams & Christ 2003 — Motion onset captures attention. *Psychological Science* 14(5). https://doi.org/10.1111/1467-9280.01458
- Smith & Abrams 2018 — Motion onset really does capture attention. *Attention, Perception, & Psychophysics*. https://doi.org/10.3758/s13414-018-1548-1
- Franconeri & Simons 2003 — Moving and looming stimuli capture attention. *Perception & Psychophysics* 65(7). https://doi.org/10.3758/BF03194829
- Sunny & von Mühlenen 2011 — Motion onset does not capture attention when subsequent motion is smooth. *Psychonomic Bulletin & Review*. https://pubmed.ncbi.nlm.nih.gov/21901513/
- Bartram, Ware & Calvert 2003 — Moticons: detection, distraction and task. *IJHCS* 58(5). https://doi.org/10.1016/S1071-5819(03)00021-1
- Altmann & Trafton 2004 — Task interruption: resumption lag and the role of cues. *Proc. CogSci*. https://www.interruptions.net/literature/Altmann-CogSci04.pdf
- Altmann, Trafton & Hambrick 2014 — Momentary interruptions can derail the train of thought. *JEP: General*. https://doi.org/10.1037/a0030986
- Iqbal & Horvitz 2007 — Disruption and recovery of computing tasks. *CHI '07*. https://doi.org/10.1145/1240624.1240730
- Adamczyk & Bailey 2004 — If not now, when? Effects of interruption at different moments within task execution. *CHI '04*. https://doi.org/10.1145/985692.985727
- Iqbal & Bailey 2008 — Effects of intelligent notification management on users and their tasks. *CHI '08*. https://doi.org/10.1145/1357054.1357070
- Mark, Gudith & Klocke 2008 — The cost of interrupted work: more speed and stress. *CHI '08*. https://doi.org/10.1145/1357054.1357072
- Dabbish, Mark & González 2011 — Why do I keep interrupting myself? *CHI '11*. https://doi.org/10.1145/1978942.1979405
- Stothart, Mitchum & Yehnert 2015 — The attentional cost of receiving a cell phone notification. *JEP:HPP* 41(4). https://doi.org/10.1037/xhp0000100
- Fitz et al. 2019 — Batching smartphone notifications can improve well-being. *Computers in Human Behavior*. https://doi.org/10.1016/j.chb.2019.07.016
- Weiser & Brown 1996 — The coming age of calm technology. https://calmtech.com/papers/coming-age-calm-technology

Cognitive load, coherence, flow
- Sweller, van Merriënboer & Paas 2019 — Cognitive architecture and instructional design: 20 years later. *Educ. Psychol. Rev.* https://doi.org/10.1007/s10648-019-09465-5
- Ginns 2006 — Integrating information: spatial and temporal contiguity meta-analysis. *Learning and Instruction*. https://doi.org/10.1016/j.learninstruc.2006.10.001
- Sundararajan & Adesope 2020 — Keep it coherent: a meta-analysis of the seductive details effect. *Educ. Psychol. Rev.* https://doi.org/10.1007/s10648-020-09522-4
- Fisher, Godwin & Seltman 2014 — Visual environment, attention allocation, and learning in young children. *Psychological Science*. https://doi.org/10.1177/0956797614533801
- Xie, Zhou & Liu 2018 — Null effects of perceptual disfluency on learning outcomes. *Educ. Psychol. Rev.* https://doi.org/10.1007/s10648-018-9442-x
- Fong, Zaleski & Leach 2015 — The challenge–skill balance and antecedents of flow: a meta-analytic investigation. *J. Positive Psychology*. https://doi.org/10.1080/17439760.2014.967799

Retrieval, generation, confidence, defaults
- Adesope, Trevisan & Sundararajan 2017 — Rethinking the use of tests: a meta-analysis of practice testing. *RER* 87(3). https://doi.org/10.3102/0034654316689306
- Bertsch et al. 2007 — The generation effect: a meta-analytic review. *Memory & Cognition* 35(2). https://doi.org/10.3758/BF03193441
- Kornell, Hays & Bjork 2009 — Unsuccessful retrieval attempts enhance subsequent learning. *JEP:LMC*. https://doi.org/10.1037/a0015729
- Pan & Carpenter 2023 — Prequestioning and pretesting effects: a review. *Educ. Psychol. Rev.* https://doi.org/10.1007/s10648-023-09814-5
- Szpunar, Khan & Schacter 2013 — Interpolated memory tests reduce mind wandering and improve learning of online lectures. *PNAS*. https://doi.org/10.1073/pnas.1221764110
- Butterfield & Metcalfe 2001 — Errors committed with high confidence are hypercorrected. *JEP:LMC* 27(6). https://doi.org/10.1037/0278-7393.27.6.1491
- Double & Birney 2019 — Reactivity to measures of metacognition. *Frontiers in Psychology*. https://doi.org/10.3389/fpsyg.2019.02755
- Liu & Conrad 2019 — Where should I start? On default values for slider questions in web surveys. *Social Science Computer Review*. https://doi.org/10.1177/0894439318755336
- Johnson & Goldstein 2003 — Do defaults save lives? *Science* 302. https://doi.org/10.1126/science.1091721

Progress and choice
- Kivetz, Urminsky & Zheng 2006 — The goal-gradient hypothesis resurrected. *J. Marketing Research* 43(1). https://doi.org/10.1509/jmkr.43.1.39
- Villar, Callegaro & Yang 2013 — Where am I? A meta-analysis of experiments on progress indicators for web surveys. *SSCR*. https://doi.org/10.1177/0894439313497468
- Hick 1952 — On the rate of gain of information. *QJEP* 4(1). https://doi.org/10.1080/17470215208416600
- Fitts 1954 — The information capacity of the human motor system in controlling the amplitude of movement. *JEP* 47(6). https://doi.org/10.1037/h0055392

Reading and typography
- Rello, Pielot & Marcos 2016 — Make it big! The effect of font size and line spacing on online readability. *CHI '16*. https://doi.org/10.1145/2858036.2858204
- Dyson & Haselgrove 2001 — The influence of reading speed and line length on the effectiveness of reading from screen. *IJHCS* 54(4). https://doi.org/10.1006/ijhc.2001.0458
- Dyson 2004 — How physical text layout affects reading from screen. *Behaviour & IT* 23(6). https://doi.org/10.1080/01449290410001715714
- Arditi & Cho 2005 — Serifs and font legibility. *Vision Research*. https://www.researchgate.net/publication/7661546_Serifs_and_font_legibility
- Piepenbrock, Mayr, Mund & Buchner 2013 — Positive display polarity is advantageous for both younger and older adults. *Ergonomics*. https://doi.org/10.1080/00140139.2013.790485
- Dobres, Chahine & Reimer 2017 — Effects of ambient illumination, contrast polarity, and letter size on text legibility under glance-like reading. *Applied Ergonomics* 60. https://doi.org/10.1016/j.apergo.2016.11.001
- Schotter, Tran & Rayner 2014 — Don't believe what you read (only once): comprehension is supported by regressions during reading. *Psychological Science*. https://doi.org/10.1177/0956797614531148
- Delgado, Vargas, Ackerman & Salmerón 2018 — Don't throw away your printed books: a meta-analysis on reading media and comprehension. *Educational Research Review*. https://doi.org/10.1016/j.edurev.2018.09.003
- Butterick, *Practical Typography* — Line length: https://practicaltypography.com/line-length.html · Point size: https://practicaltypography.com/point-size.html

Learning map (§7.6)
- Purchase 1997 — Which aesthetic has the greatest effect on human understanding? *GD '97*, LNCS 1353. https://doi.org/10.1007/3-540-63938-1_67
- Sugiyama, Tagawa & Toda 1981 — Methods for visual understanding of hierarchical system structures. *IEEE SMC* 11(2). https://doi.org/10.1109/TSMC.1981.4308636
- Gansner, Koutsofios, North & Vo 1993 — A technique for drawing directed graphs. *IEEE TSE* 19(3). https://doi.org/10.1109/32.221135
- Burch et al. 2011 — Evaluation of traditional, orthogonal, and radial tree diagrams by an eye tracking study. *IEEE TVCG* 17(12). https://doi.org/10.1109/TVCG.2011.193
- Misue, Eades, Lai & Sugiyama 1995 — Layout adjustment and the mental map. *JVLC* 6(2). https://doi.org/10.1006/jvlc.1995.1010
- Amadieu, van Gog & Paas 2009 — Effects of prior knowledge and concept-map structure on disorientation, cognitive load, and learning. *Learning and Instruction* 19(5). https://doi.org/10.1016/j.learninstruc.2009.02.005
- Nesbit & Adesope 2006 — Learning with concept and knowledge maps: a meta-analysis. *RER* 76(3). https://doi.org/10.3102/00346543076003413
- Rosenholtz, Li & Nakano 2007 — Measuring visual clutter. *Journal of Vision* 7(2):17. https://doi.org/10.1167/7.2.17
- Haroz & Whitney 2012 — How capacity limits of attention influence information visualization effectiveness. *IEEE TVCG* 18(12). https://doi.org/10.1109/TVCG.2012.233
- Mitrovic & Martin 2007 — Evaluating the effect of open student models on self-assessment. *IJAIED* 17(2). https://doi.org/10.3233/irg-2007-17(2)03
- Long & Aleven 2017 — Enhancing learning outcomes through self-regulated learning support with an open learner model. *UMUAI* 27(1). https://doi.org/10.1007/s11257-016-9186-6
- Kornell & Metcalfe 2006 — Study efficacy and the region of proximal learning framework. *JEP:LMC* 32(3). https://doi.org/10.1037/0278-7393.32.3.609
- Kornell & Bjork 2007 — The promise and perils of self-regulated study. *Psychonomic Bulletin & Review* 14(2). https://doi.org/10.3758/BF03194055
- Rohrer & Taylor 2007 — The shuffling of mathematics problems improves learning. *Instructional Science* 35(6). https://doi.org/10.1007/s11251-007-9015-8
- Brunmair & Richter 2019 — Similarity matters: a meta-analysis of interleaved learning and its moderators. *Psychological Bulletin* 145(11). https://doi.org/10.1037/bul0000209
- LearnHarness v0.4.1 (look and interaction studied, no code or text reused; AGPL-3.0): https://github.com/amosblomqvist/learnharness — its graph is a force-directed canvas behind Home with session-coloured clusters; its plan is a layered top-down DAG; practice ideas are picked from a checkbox list ("Chains"), not from the canvas.

Standards and platform references
- W3C, WCAG 2.2: https://www.w3.org/TR/WCAG22/ — Understanding docs used:
  - 1.4.3 https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html
  - 1.4.11 https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html
  - 1.4.1 https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html
  - 1.4.10 https://www.w3.org/WAI/WCAG22/Understanding/reflow.html
  - 1.4.12 https://www.w3.org/WAI/WCAG22/Understanding/text-spacing.html
  - 2.2.2 https://www.w3.org/WAI/WCAG22/Understanding/pause-stop-hide.html
  - 2.3.3 https://www.w3.org/WAI/WCAG22/Understanding/animation-from-interactions.html
  - 2.4.7 https://www.w3.org/WAI/WCAG22/Understanding/focus-visible.html
  - 2.4.11 https://www.w3.org/WAI/WCAG22/Understanding/focus-not-obscured-minimum.html
  - 2.5.8 https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html
  - 4.1.3 https://www.w3.org/WAI/WCAG22/Understanding/status-messages.html
- MDN — `prefers-reduced-motion`: https://developer.mozilla.org/en-US/docs/Web/CSS/@media/prefers-reduced-motion · `overflow-anchor`: https://developer.mozilla.org/en-US/docs/Web/CSS/overflow-anchor
