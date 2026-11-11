# P's Learning Harness UI — "Carrel"

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
- **Implication:** one column. The question sits in the flow, directly after the text it asks about (contiguity). The plan is a reference you open on demand. No avatars, no per-turn metadata, no decorative chrome. Maximum legibility. "One column" is about *one* reading axis, not a narrow one: the column follows the window (§5.2), because a short measure stranded in a wide window is itself a split-attention cost — the return sweep is longer than the line.

### 2.4 Flow conditions — *Moderate*
- Flow correlates with perceived challenge–skill balance. Clear goals, unambiguous feedback and a sense of control also correlate [Fong 2015]. These are correlations, mostly from self-report. A UI cannot create flow, only avoid blocking it.
- **Implication:** make the *current goal* visible: each section opener shows the section's exit goal, and the running head shows the section label. Make *feedback* immediate and unambiguous: the verdict appears in place, as words, next to the learner's own confidence. Give *control*: the learner scrolls, the learner can stop, and nothing moves on its own.

### 2.5 Reading typography — *Mixed: some strong, some contested*
- **Size:** larger sizes (18 pt and up in their setup) improved readability and comprehension for 104 readers. Extreme line spacings (0.8 and 1.8) hurt [Rello 2016]. Viewing distance changes the absolute numbers, so treat this as "bigger than the 14–16 px habit". *Moderate.*
- **Measure:** ~55 cpl gave the best comprehension at normal and fast reading speeds in one study [Dyson & Haselgrove 2001]. The review literature is mixed: longer lines are sometimes read faster, while moderate lengths are preferred [Dyson 2004]. Typographic practice puts it at 45–90 characters [Butterick, line length]. *Moderate/contested.* I target ~60–72 characters where the window is narrow, and let the column grow on a wide screen (§5.2): the owner reports that a fixed short measure made reading *harder* on a large display, because the eye has to find the start of a short line far from the last word of the previous one. The wide-screen step is owner-requested (2026-11-07) and sits at the top of the contested range, not beyond what the evidence says is safe for a paragraph of this size.
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
| Question or choice pending | The exercise block | Running head; desk note "Question waiting." | Composer (hidden, draft kept) |
| Feedback | Verdict and explanation, in place, under the learner's answer | — | — |
| Planner working | The last turn (re-readable); job line "Planner — drafting" | Head "Planning…"; desk note | Composer |
| Plan approval | The plan document | Head "Plan · review"; desk note | Composer |
| New section | The section opener (title and exit goal) | Head "Section n of m" | — |
| Looking up the plan | Plan overlay (full screen) | Session blurred behind | — |
| Error | Inline error block with Retry, at the end of the flow | — | Toasts (removed) |

### 3.2 Removed, hidden, peripheral

- **Removed:** sidebar in sessions; avatars; per-reply model and author metadata; model pill and status dot; mode pills and dots; badges and pills; gradients; drop shadows; spinners; typing dots; caret blink; toasts; suggestion chips; the uppercase accent kickers; the second header band.
- **Hidden until asked:** the session list (Home only), the plan (sheet), the learning map (its own page, reached from Home only), research briefs (disclosure), settings (Home only, plus a link inside model-related errors), tutor reasoning in Practice (closed disclosure before the reply).
- **Peripheral and static:** the running head (back, title, phase position, Plan) and the desk (composer, Stop, or the "continue" note).

---

## 4. Principles

Each principle is binding. The evidence keys refer to §2.

- **P1. One center.** At any moment exactly one region invites action: the desk composer, *or* the pending exercise, *or* the plan document, *or* an open sheet. Everything else is static. (§2.3, §2.8) An interactive view in the flow (§7.2.15) that is not an answer surface is **part of the page, not a second center**: it is the figure the sentence beside it refers to, and moving its controls is reading that figure, so the desk composer stays the one place to act and nothing the view does changes the desk. A view in answer mode *is* the pending exercise. [learning] Animations and simulations help when the learner is told what to manipulate (Höffler & Leutner 2007, doi:10.1016/j.learninstruc.2007.09.013; Rutten, van Joolingen & van der Veen 2012, doi:10.1016/j.compedu.2011.07.017); placing the view inside the prose that does the telling keeps instruction and manipulation in one place (§2.3 split attention).
- **P2. Nothing moves that the learner did not move.** No autoscroll during streaming. No looping animation anywhere. No sliding-in. The only animation is the open and close of a sheet the learner requested, and it is removed under `prefers-reduced-motion`. (§2.1, §2.5 Schotter)
- **P3. Background work stays in the background.** Job progress updates in place, as text. Anything that needs the learner (plan ready, question pending, failure) is surfaced at a turn boundary in the desk. It never takes focus away from where the learner is reading. (§2.2)
- **P4. The page is a page.** The tutor's words are set like a book: serif, 20 px, line-height 1.55, ragged right, on one measured column that follows the window (56 ch on a narrow screen, up to 104 ch on a wide one — §5.2; owner-requested, 2026-11-07). The learner's words are set as notes: sans, indented, labelled "You". There are no bubbles, cards or avatars. (§2.3, §2.5)
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

- `--measure` is in `ch` and is applied once, as `inline-size: min(var(--measure) + 2*var(--gutter), 100%)` on the shared `.col`. Because the column's `font-family` is `--font-read`, `ch` tracks the serif in use.
- **It steps with the viewport** (owner direction, 2026-11-07): `56ch` below 1024 px, **`76ch` ≥ 1024 px, `92ch` ≥ 1280 px, `104ch` ≥ 1600 px**. The owner's words: the column used to be "a narrow column in the middle" and "it makes reading a bit hard" on a wide screen — at 1280–2560 px a 56ch strip leaves most of the window empty and the line return is hard to find. The steps are in `styles.css` next to the `.col` rule, so the flow, the running head, the desk, Home and (through it) every other column widen together; nothing else sets a measure.
- **Measured in Chromium at 20 px** (DejaVu Serif in the headless build, 11.18 px per `ch`):
  - The old fixed rule: 56 ch → **626 px**.
  - 1280 px viewport → 92 ch → **1028.5 px** of prose.
  - 2560 px viewport → 104 ch → **1162.7 px** of prose (still 45 % of the window).
  - Prose stays a text column, never full bleed, and the widest step is still well inside the window at every width since the window is the `min()`'s other side only below the step.
- **What does not change:** code blocks and tables keep `overflow-x: auto` (§7.2.15, `.prose pre` / `.prose table`), the artifact figures keep `max-inline-size: 100%`, and the Settings sheet keeps its own 640 px column. Widening the flow never wraps a table or squeezes a view.
- **Acceptance range (§12 A4): 58–72 characters per line below 1024 px, ~90–100 above 1280 px.** Long lines on a wide screen are the researched *moderate/contested* case (§2.5): what the owner reports losing is the line return, not the words, and a column that fills a wide window restores it.
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
--measure: 56ch; /* ≥1024px: 76ch; ≥1280px: 92ch; ≥1600px: 104ch (§5.2) */
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
│        ┌─ page column: --measure, centered (56ch → 104ch) ─────┐  │
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
│        │ Question waiting.                    [Go to question] │  │
│        └──────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────────┘
```

- The scroller is `#messages` (keep the id). The running head and the desk are outside the scroller; they are sticky siblings in a grid `grid-template-rows: auto 1fr auto`.
- The running head, page and desk share one inline axis: `inline-size: min(var(--measure) + 2*var(--gutter), 100%)`, centered. The eye never has to travel to the viewport corners. `--measure` steps with the viewport (§5.2): the axis widens on a big screen instead of leaving a narrow strip in the middle of it (owner direction, 2026-11-07).
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
│ [Reply to the tutor ] [Send] │  desk; padding-bottom: var(--safe-bottom)
└─────────────────────┘
```

Use `height: 100dvh` so the desk stays above the on-screen keyboard.

**Safe areas.** The page is edge-to-edge (`viewport-fit=cover`), so on a phone it runs under the status bar, the notch and the gesture bar. `--safe-top/right/bottom/left` (`env(safe-area-inset-*)`, 0 on desktop; Android WebView reports them from M144) keep every control clear of them:
- `.app` pads top and sides, so Home, the session and the Map start below the status bar; paper shows behind it.
- The bottom inset goes on what touches the bottom edge: the desk, the end of Home's list, and the bottom-sheet drawers.
- Full-screen sheets pad themselves, because they sit in the top layer outside `.app`. Settings uses a paper-coloured **border** of the inset width rather than padding, so it scrolls under a fixed strip instead of behind the status bar. The plan overlay and its side drawers pad by the insets.
- `scripts/ui-check/run.sh sync` checks this with an emulated 47 px status bar and 34 px gesture bar.

### 6.2 Running head

Contents, left to right:

1. **Back button.** Text "← Sessions" on desktop. On phone, a 44×44 icon button `aria-label="Back to sessions"`. It goes to Home. While a Practice reply is streaming, navigation stays blocked as today: show inline status text in the head, "Wait for the reply to finish, or stop it.", for as long as the condition holds. No toast.
2. **Title.** An `h1`, `--fs-ui` 600, `--ink-2`, single line with ellipsis. It must remain visible at 320 px. It may shrink to one word plus ellipsis, but it is never dropped (fixes D4).
3. **Phase position.** A `nav aria-label="Session progress"` containing an `ol` of the four phases. Learn only, not read-only.
   - **Desktop/tablet** shows all four as text: `Measure · Plan · Teach 2/5 · Close`, when they fit.
     - Past phases: `--ink-2`.
     - Current phase: `--ink` 600 with `aria-current="step"`.
     - Future phases: `--ink-3`.
     - Separators are `·` with `aria-hidden`.
   - **Phone** shows the current phase label only; the other three are `.sr-only`.
   - **Title first.** On any width, when the full list would truncate the title or wrap the head onto a second line, the list shows only the current phase (the same `.sr-only` rule, as `.phase.compact`). `fitHead()` decides this from a `ResizeObserver` on the head row, the title and the list, so it follows window size, title changes, label changes and extra head items (Plan, workspace chip, More). Reason: the head is a fixed 674 px column at every desktop width, so a viewport breakpoint can't tell when it is full; at 1280×860 a Learn session awaiting approval truncated "Derivatives" to "Deriv…" and pushed "More" to a second line. The title says *which* session this is and the current phase says *where* it is; the past and future phase names are the least needed and are still announced.
   - Label rules:
     - Measure → "Measure".
     - Plan while the planner is running → "Planning…". This is static text, no spinner; it keeps `role="status"` from today so screen readers hear it once.
     - `awaitApproval` → "Plan · review" (was "Plan · awaiting approval"; the long label caused most of the overflow above).
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
| **Note: exercise pending** | A `question_ask` or `choice_ask` is pending. | Text "Question waiting." plus a secondary button "Go to question", which scrolls the exercise's first field into view (`block: "center"`, instant) and focuses it. The composer element is hidden (`hidden`), not cleared; its draft is kept. |
| **Note: plan** | `phase === "plan"` with the planner running. | "Drafting your plan…" No button. |
| **Note: approval** | `phase === "awaitApproval"`. | "Plan ready." plus the secondary button "Go to plan". |
| **Ended** | `session_state.ended` (Learn, not read-only). | "Session complete." plus the primary button "Practise these ideas" (§7.9). With no plan nodes: "Start a new session" instead. |
| **Read-only** | `SessionSummary.readOnly` with `readOnlyReason` `"legacy"`. | "Made with an older tutor. Read-only." plus the primary button "Start a new session" (goes to Home in Learn mode). |
| **Read-only: desktop plugin** | `readOnly` with `readOnlyReason === "desktop-plugin"` (on a phone or tablet, the session used a plugin that only runs on desktop; `docs/sync-build.md`, "Sessions on mobile"). | "⟨names⟩ only runs on desktop. Continue this session on desktop." (`desktopOnlyPlugins` joined "A, B and C"; "run" for more than one) plus the secondary button "Continue here anyway", which calls `mobile_unlock({ sessionId })` and re-opens the session, now writable. On failure the note reads "Couldn’t continue here: ⟨error⟩". |

The notes are status, not instructions: the button next to each one already says what to do, so the note only names the state (copy rule (a), §7.10).

- A rejected send (`send_learner_message` rejects) puts the text back in the textarea, as today. The rejection message appears directly under the textarea in `--danger` text, `--fs-small`, with `role="status"`. It stays until the next input event or the next successful send.
- There is no visible "Enter to send / start" hint anywhere, on Home or in sessions. The keys are standard; they are exposed only as `aria-describedby` text on the input (`.sr-only`).
- Hiding the composer follows today's backend rules exactly: anything the backend would reject (pending card, plan phase, read-only) gets a note instead of an input.
- The composer's attachment chips and `@` list sit above the textarea inside the same desk block (§7.2.13). A hidden composer hides them too; drafts and pending chips are kept. The workspace chip is in the running head (§7.2.14).

**Comment (added 2026-11-11).** Under the desk note or the streaming composer, a text button "Comment" opens one
line to the tutor (`#desk-comment`) whenever the learner can't simply reply: a question or choice is pending, a tutor
reply is streaming, or the plan waits for approval. Not while the planner drafts, not when ended or read-only. Enter
sends, Shift+Enter breaks the line, Escape closes. It calls `comment({ sessionId, text })`: the backend stops a reply
in flight, closes a pending question or choice with a `setAside` result (the card then reads "Set aside for your
comment."), appends the comment as an ordinary learner message and runs the tutor on it, which answers it first. With
the plan awaiting approval the comment becomes a plan change request. A refused comment reopens the box with the text
and the reason under it. The tutor records how-to-teach instructions with `preference_add`, so they hold for the rest
of the session.

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
| **`nudge`** (UI-only, never in the op log) | §7.8 |

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
 P's Learning Harness                                       Map   Settings   ⟳•
 (wordmark, --fs-ui 600 --ink-2, plain text)    (text buttons; the sync icon only when a target is set)

                                                    ┌─────────────────────────────┐
                                                    │ Git · Last synced 5 min ago │ ← the icon's panel, open on
                                                    │ WebDAV · Not synced         │   click, top-right corner at
                                                    │ the server …  (--danger)    │   the icon's bottom-right
                                                    │ 2 files need a look         │
                                                    │ [Sync now]  Sync settings   │
                                                    └─────────────────────────────┘

            ┌────────────────────────────────────────────┐
            │ CONTINUE                                   │ ← secondary button, only when the
            │ Derivatives                  Plan · review │   newest session can go on (§7.9)
            └────────────────────────────────────────────┘
            What do you want to learn?          ← --fs-display, serif, left-aligned in column
            (•) Learn   ( ) Practice
            ┌────────────────────────────────────────────┐
            │ A topic or goal…                            │ [Start]
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

- **Column:** the same `--measure` column as the session page, so it widens with it (§5.2). The top bar holds the wordmark (plain text, no logo mark), two text buttons, "Map" (§7.6) and "Settings", and the sync icon. "Map" is hidden when there is no app backend. Vertical offset: `--s9` above the display line on desktop, `--s7` on phone.
- **One line at every width.** The bar never wraps (`.home-bar` is exactly `--head-h` tall). On touch the text buttons' own padding is the spacing, and the icon's 44 px target reaches 13 px into the gutter so its glyph lines up with the column edge. The wordmark keeps its full name at 390 px. Below that it is the part that gives way, ending in an ellipsis ("P's Lear…" at 320 px); the links never do.
- **Sync icon** (last in the bar, only when git or WebDAV is configured, §7.5.2): an icon button holding an inline SVG of the two-arrow sync glyph, 18 px, `stroke="currentColor"`, no fill, `aria-hidden`. It takes the links' colour in both themes, the existing focus ring, a `--raised` hover fill, and a 44 px target on coarse pointers. State is shown without motion. "Not synced" (a configured target has a last error or unsent changes) adds a 7 px `--danger` dot at the glyph's top-right; that dot is the only colour. While a sync started from this window runs, the icon has `aria-busy="true"` and turns `--ink-3`. Otherwise the icon is plain. Its accessible name is "Sync: ⟨Syncing… | Not synced | Synced ⟨just now | 5 min ago | 3 h ago | yesterday | on 3 Nov⟩⟩" from the newest `lastSyncAt`, and its `title` gives the full time. It hides when nothing is configured or `sync_status` fails; the failure is not shown.
- **Sync panel:** the icon is a disclosure (`aria-expanded`, `aria-controls`) for a small panel. The panel is the session head's `.menu` dropdown, right-aligned under the icon, at most 280 px and never wider than the column, so it stays inside the viewport at 320 px. It holds:
  - one line per configured target ("Git · Last synced 5 min ago", "WebDAV · Not synced · last 3 h ago"), with that target's last error under it in `--danger`, small and wrapping;
  - "⟨n⟩ files need a look" when the needs-attention list isn't empty;
  - a secondary "Sync now" button that reads "Syncing…" and is disabled while it runs, with a status line below: the report ("Synced. Git: … WebDAV: …"), or, after a failure, which targets failed and which synced ("Git didn't sync (see above). WebDAV synced."). Each target's own error is already under its name, so the line never repeats one; an unlabelled error under the button read as the last target's (owner, 2026-11-09). The raw error shows only when no target recorded one;
  - a "Sync settings" text button. It closes the panel and opens Settings with Sync expanded, focus on its heading button.

  Opening the panel re-reads `sync_status`, and a refresh or a sync redraws it in place, so it stays open. Escape closes it and returns focus to the icon; a click outside or on the icon closes it too, and leaving Home closes it.
- **Sync banner** (Home only, never in sessions): above "Your sessions", `role="status"`, in the `sessions-error` style. "⟨Target⟩ not synced: ⟨its last error⟩" (the first failing target, e.g. "Git not synced: …") in `--danger` with a "Try again" text button (runs `sync_now`), and/or "⟨n⟩ files changed on two devices need a look." in `--ink-2` with a "Review" text button (opens Settings at the needs-attention list). Hidden when neither applies.
- **Sync refresh:** `sync_status` is read when Home shows, every 60 s while Home is visible and the window is in front (no timer otherwise), when Settings opens and after every sync command.
- **Background sync:** when the document becomes hidden (`visibilitychange` → `hidden`; on mobile this is the app going to the background) the UI calls `sync_if_changed()` once, fire-and-forget. The backend syncs only when a configured target has local changes; nothing is shown.
- **Mode:** a real radio group (`fieldset` + `legend` "Mode", visually hidden legend), replacing `role="tablist"` (wrong semantics). The two options sit on one line and show their names only (`--fs-ui` 600), with no description. The display line already says what each mode is for ("What do you want to learn?" / "…to practise?"), so a description restated it (copy rule (a), §7.10); cutting it also keeps the start area to the question, the choice and the input. Switching mode updates the display line and placeholder from today's `COPY` ("A topic or goal…" / "A topic or problem set…").
- **Continue:** see §7.9.
- **Input:** the same `#composer` element moved into the home slot (keep today's slot mechanism). Its label is the display line (`aria-labelledby` = `#hero`). The button reads "Start" (primary).
- **No suggestions.** There is no "Or try:" line. Example topics are content the learner did not ask for (a seductive detail, §2.3) and a second set of targets next to the one input (§2.8).
- **Setup notice** (no model ready): a paragraph above the input, "Connect a model to start." followed by the text button "Settings". Start stays enabled. Pressing it without a model shows the inline status "Choose a model first." under the input and opens Settings (today's toast becomes inline).
- **Session list** under the heading "Your sessions" (`h2`, `--fs-ui` 600):
  - Groups Today / Yesterday / Previous 7 days / Older, as small-caps `h3` in `--fs-meta` `--ink-3`.
  - Each row is an `li` containing an open button (title in `--fs-ui` `--ink`; meta "Learn · 10:42", "Practice · Tue" or "Read-only · 1 Jan" in `--fs-small` `--ink-3`, right-aligned) and a sibling "…" text button (`aria-haspopup="menu"`, labelled "More actions for ⟨title⟩") that opens the row's menu.
  - **Locked** (on a phone, `readOnlyReason === "desktop-plugin"`, docs/sync-build.md "Sessions on mobile"): the meta reads "Locked · 10:42" instead of "Learn"/"Read-only", and the row gets `.locked`, which greys the title to `--ink-3` (the same dimming as a known plan node). The open button stays enabled; its accessible name (title + meta) includes "Locked", and opening it shows the desktop-plugin desk note with "Continue here anyway" (§6.3). Legacy read-only rows are unchanged.
  - Row height is at least 48 px.
  - "…" is visible on `:hover`/`:focus-within` (and while its menu is open) for `(hover: hover)` devices, and always visible on `(hover: none)`.
  - The menu (`.menu`, right-aligned under "…"; arrow keys, Home/End, Escape back to "…") holds two items:
    - **Extract** runs the graph builder now over the session's teaching not yet in the map (`graph_build`, architecture §4.7). Opening the menu asks `graph_status`; the item stays greyed ("Checking…") until it answers, and stays greyed while a build is running ("Running now") or while fewer than a quarter of the automatic threshold's tutor tokens are new ("312 of 1,024 new tokens needed"). Enabled, its note reads "2,380 new tokens". Pressing it closes the menu and the row's status line reads "Extracting into the map in the background." in `--ink-3`; a refusal reads "Couldn’t extract this session: …" in `--danger`.
    - **Delete**: the first press arms it ("Delete? Press again", `aria-live` polite); a second press within 4 s deletes. Closing the menu disarms it.
  - Incremental updates stay exactly as today: one entry per `session` event, moved or updated in place; no re-list.
  - Empty state: "No sessions yet." in `--ink-3`.
- **Errors:**
  - `list_sessions` failure: inline under "Your sessions": "Couldn't load your sessions: ⟨message⟩", with a text button "Try again".
  - `subscribe_events` failure: a one-line notice at the top of Home in `--danger` text: "Live updates are unavailable: ⟨message⟩". It persists.
  - Not inside the app (`!available`): the input area is replaced by "Open this in the P's Learning Harness app."

Phone: the same stacked order. The display line is 28 px. Session-row meta wraps under the title. Delete is always visible.

#### 7.1.1 Branch sessions (tracks that fork and merge)

The planner may split a plan into parallel **tracks** (`plan.tracks`) that later merge. Approval
keeps the first root track in the session, forks one new session per other root track, and the end
of the last input branch creates a **merge session** that opens with a revision section. New
sessions never run on their own: they wait in the list until the learner opens them. A plan without
tracks and a session without `lineage`/`branch` render exactly as before.

- **Session row.** A row whose summary carries `lineage` gets a second line under the title, inside
  the open button (so the row's accessible name includes it), `--fs-meta` `--ink-3`: "Branch 2 of 3 ·
  Geometry · from ⟨origin title⟩" (the origin row omits "from"), or "Merge of Algebra + Geometry".
  After it, a state chip (`--hair` border, `--r`): **Ready** (never run; `--accent` text and border),
  **In progress** (`--select` fill), **Done · merge waits for ⟨tracks⟩** / **Done · merge pending**
  (ended, merge not created yet), **Done · merged**, or **Done** (the final track). Every `session`
  event refreshes the line in place. The row switches to a grid so a long title still ellipsizes
  beside the meta; on a phone the line stacks like the meta.
- **Start.** Opening a Learn session whose `lineage.started` is false (phase teach, not ended, no
  pending call, no tutor turn) shows the inline status bar of §7.2.11 (`role="status"`): "This
  branch is ready: ⟨track⟩." or "The merge session is ready: it starts with a short revision.", with
  one secondary button **Start** (`resume_turn`). Sending a message works too and clears the bar.
- **Branch notice.** A `branch-notice` message (`[branches] …`) is a flow notice like §7.2.7, with
  the small-caps label "Branches" on its own line and the text rendered as Markdown in `--fs-small`
  `--ink-2` (the merge session's notice summarises each input branch in several paragraphs).
- **Plan diagram lanes.** With tracks, the diagram draws one lane per track: a dashed `--rule`
  frame with the track title in small `--ink-3` 600 text at its top-left. Root lanes sit side by
  side and share the width; a merge lane sits centred under the lanes it joins, with a dashed join
  arrow from each input lane's bottom into its top. Each lane is laid out with the usual layered
  pass on its own sections, a revision section on top; prerequisite arrows are drawn only inside a
  lane (between lanes the join arrow stands for them). In a branched session (`st.branch`), its own lane is outlined in
  `--accent`; the other lanes, their boxes and links drop to 50 % opacity and their titles read
  "⟨track⟩ · other session" (box tooltips add "taught in another session"). A revision section's
  box has a dashed outline and the meta "Revision · ⟨n⟩ ideas" (the ideas it re-checks — nodes owned
  by other sections, so a node's home is always its non-revision section).
- **Plan card.** Under the drawing, a "Tracks" list: per track, its title (merge tracks add "— after
  A + B") and its sections in teaching order joined by "→", revision sections as "Revision: ⟨title⟩".
  Above the actions, while the plan is a proposal with two or more root tracks: "This plan runs as N
  parallel tracks. Approving keeps "⟨first track⟩" here and creates N−1 more sessions, one per
  track; they run when you open them from Home." (+ the merge sentence when the plan merges); Approve
  is `aria-describedby` it. The drawer of a revision section says what the revision is for.
- **Plan sheet and progress.** The overlay's Sections drawer groups sections under one `h4` per
  track ("— after A + B", "— this session" / "— other session"; other tracks in `--ink-3`), and
  marks revision sections "— Revision". In a branched session every count — the head's "Teach 2/4",
  "Plan (Teach, section n of m)", the overlay's "Section n of m" and the current-section fallback —
  counts only the session's own track. A section opener counts within its section's track and
  prefixes "Revision · " for a revision section.

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
  - Validation for an empty answer when not using "I don't know": "Write an answer or pick “I don't know”."
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
 ✓ Correct.                                      ← verdict line, --font-ui 600 --ink
 The slope of the tangent at x = 1 is 2 …        ← args.explanation, when the question has one, --font-read
```

- Verdict words are unchanged ("Correct", "Partly right", "Not quite"). Each gets a leading glyph `✓` / `◐` / `✗` (`aria-hidden`), always in `--ink`. No green/red, no pill.
- The verdict line is the verdict alone. The answer, reasoning and confidence sit right above it, so it doesn't repeat them. The `verdict_record` note is the tutor's diagnosis for the planner, compaction and its own later turns, and is never rendered; the tutor's reaction to the answer arrives as its next message, below the block.
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
  - the desk note "Planning failed. Describe your goal again." with the composer shown, if the backend accepts messages in that state, or the note "Planning failed." alone if it does not.

#### 7.2.9 Plan card (await approval) and the plan sheet (reference while teaching)

**Plan card** (`#plan-slot`, after the timeline, phase `awaitApproval`). Rendered as a document,
not a boxed card: a top `--hair` and `--s7` spacing. **Owner direction, 2026-11-07: the card *is*
the flowchart.** It used to be a topic drawing followed by a long list of editable section blocks;
the owner's verdict was "i dont like how it shows the flowchart then a massive wall of text where you
can edit. i want you to change it so its a flow chart with a button in the corner to expand the view
to the whole screen. then each square in the flowchart has the 3 dots inside it which opens a little
frame maybe on the side of the screen that shows the long description and the actions you can take."
The list is gone: the drawing, its boxes' own state, and one side drawer per box carry it all.

```
 PROPOSED PLAN                                                 Expand ← corner button, top right
 Understand derivatives                            ← h2, --fs-h1, --font-read (the goal) ⋯ ← plan options
 Start from slopes of secants, then take the limit. ← approach, .prose

 ┌─ plan-figure (one box per topic; drawn larger than the sheet's overview) ─┐
 │   ╭ From secants to tangents  ⋯ ╮        ╭ Power rule  ⋯ ╮               │  ← title, then the "⋯" *inside*
 │   │ 4 ideas · aim: Use         │───────▶│ 2 ideas · Done  │              │     the box's own corner
 │   ╰────────────────────────────╯        ╰────────────────╯               │  ← badges: state in words
 │  One box per topic; an arrow joins two topics wherever an idea needs…     │
 └───────────────────────────────────────────────────────────────────────────┘

 [Approve plan]   Send changes   Discard   Suggest a change
```

- **The plan is drawn as the knowledge structure it is** (`ui/plan-diagram.js`). Plan nodes *are*
  knowledge claims (`graphNode` on each node) and `requires` *are* prerequisite chains, so the card
  draws the topic view: one box per section, an arrow wherever a claim in one topic needs a claim in
  another. Before this, the card was an ordered list only, which hid the branching the planner
  spent its tokens on; a list also reads as a sequence, and the plan is a graph with several
  prerequisites per claim (13 of the 49 claims in the imported plan have two or three).
- **The drawing is the primary surface.** Only the plan's one-line approach text and the actions
  sit around it. The per-section paragraphs (mode · style · aim, Goal, Covers, notes, Tools) moved
  into the side drawer, one per box (§7.7.1).
- **The corner "Expand" opens the whole-screen plan** (the overlay below), on the same drawing —
  the card's corner button passes `{ fromCard: true }`, which forces the *Topics* level and shows
  the overlay even while the plan is a proposal. Its accessible name is "Expand the plan to the full
  screen"; closing it returns focus to the button.
- **State is drawn on the box, not in a paragraph** (§7.7.1): the one teaching *is* is outlined in
  the gold mark, a finished one is shaded, a known one is greyed and says "You know this"; the aim
  reads `aim: ⟨Level⟩`; the learner's own pending choices replace it (`Analyze only`, `Not needed`);
  "Hunt on" and "Needs install" appear when they apply, and a refuted known section is back to
  normal. Nothing is "current" before approval: the boxes show the plan's state, not where teaching
  would start.
- **Sizing and motion:** boxes are sized from a per-character width estimate (no canvas
  measurement, so the layout is deterministic), layers are longest-path with a barycenter sweep,
  and wide layers wrap into rows so nothing runs off the column. The diagram is static — no arrow
  animates (§8) — and renders at its natural size (1:1 text) inside a scroller capped at 52 vh. The
  card draws at `scale: 1.3` (≈16 px box text) because the drawing is the page there; the layout is
  re-measured to the card's own width with a `ResizeObserver`, so the text never scales up with the
  window and never shrinks below its size.
- **Colour is never the only signal:** every mark on a box is also a word, and the same facts are in
  the box's drawer.

- **Tools:** each section's `plugins` render as a `fieldset` (legend "Tools for ⟨section title⟩", visually shown as "Tools") of checkboxes, all checked initially, inside the box's drawer: a change to a checkbox is a plan edit (the section's `plugins` in `edit_plan`, the list the learner kept) and is sent with "Send changes". Approve therefore sends `removedPlugins: []` — it can no longer remove a tool by itself, and it is disabled while edits are pending.
- **Suggest a change:** a text button that reveals (no animation) a labelled textarea "What should change, and why?" with the secondary button "Send to planner" and a "Cancel" text button. Today the textarea is always visible; this progressive disclosure keeps one primary action. The draft is kept (`planDraft`). After a successful send: status "Sent. Revising the plan…"; the textarea clears. While a revision is pending, the card's status reads "Revising. Approve when it arrives."
- **Approve:** "Approving…" → the card is removed when the phase changes (unchanged). The "Plan approved …" notice arrives via the op log.
- **New `planVersion`:** re-render in place, reset the pending edits and tool checkboxes, and if focus was inside the old card, focus the new heading.
- **Learning-system controls (built; §7.7.1–§7.7.3):** the plan-level "⋯" (Depth / Explore / Sitting / Style) stays beside the title, the planner's question stays above the drawing, and every section's actions are in its box's drawer. Edits are held in `s.planEdits` (`cards.js::newPlanEdits`) and survive a re-render; a plan-version change clears them. The open drawer is remembered on that object (`drawer`), so a card rebuilt by a state refresh keeps the section the learner was working on.

**Plan overlay** (teach, close or ended — opened by the head's "Plan" button; also opened while a
plan awaits approval by the card's corner "Expand", which forces the *Topics* level and reads
"⟨N⟩ sections · not approved yet" in its bar). Owner direction,
2026-11-07: **the plan takes the whole screen and the session blurs behind it.** It was a 420 px
rail holding a scrolling diagram — the one surface the learner opens on purpose was the smallest
one in the app, and a diagram read through a letterbox is not a plan.

- **Shape:** a full-viewport modal `<dialog>` (`100vw × 100dvh`), paper at 94 % over the session,
  `::backdrop` = ink at 55 % + `backdrop-filter: blur(14px)`, so the conversation is legible as
  context and unusable as a target. Opening fades 200 ms; `@starting-style` scales 0.99 → 1. Esc,
  "Close" and a click on the backdrop close it; closing returns focus to the "Plan" button.
- **Bar:** "Plan" (the dialog's heading) followed by where we are — `Section 5 of 9 · Unit circle
  and trig derivatives`, or `N sections · finished` — then `[Topics | Ideas]`, `Sections`, `Close`.
  The level toggle is a two-button group with `aria-pressed`; the choice is remembered per session
  (`panelLevel`).
- **Stage:** the diagram at natural size, filling everything between the bar and the hint, with
  pan (drag), zoom (wheel/pinch about the pointer, 0.05–2.6×, floor = fit), arrow keys when the
  stage has focus, and `[−] [+] [Fit]` in the corner. The stage is a `role="group"` with an
  `aria-label` that says how to move it. Hint line: "Drag to pan · scroll to zoom".
- **Topics vs Ideas:** `ideas` (default) is the plan's claim DAG — every claim in the plan with its
  `requires` arrows (49 boxes, 35 arrows on the imported plan); `topics` is the overview — one box
  per section, an arrow wherever a claim in one topic needs a claim in another (the shape is
  naturally sparse: the graph has 4 cross-topic edges, §7.6).
- **Where we are, marked once:** the current section's boxes get a gold outline and a static glow
  (`#C08A2A`, one saturated mark on the whole surface); finished sections' claims are shaded
  (`--raised`) in the ideas view; the topics view marks the finished topics' boxes and the current
  one gold. "Current" is the open section, or — when a plan's current topic has no claims behind it
  — the next unfinished section, the same rule the head and the desk use. **No claim ever gets a
  tick**, because plan evidence is per topic (§7.6); the section list keeps the ✓ Done / • Current
  words.
- **Open where teaching is:** the overlay fits the diagram, and when the fit lands below 0.75 it
  frames the current section's boxes at 0.95 instead. A 49-claim DAG at 50 % is a texture; the
  learner came to see where they are.
- **Sections drawer:** `Sections` slides a 420 px panel (a bottom sheet on phones) over the stage
  with the goal, the ordered section list (title, status in words, its claims, `lastNodeId` in 600
  weight with `aria-current`), "Tools in use: …", and the "Change plan" form. `aria-expanded` on
  the toggle tracks it; the drawer is closed by default so the first thing on screen is the plan.

Behavior:

- Closed by default at every width; re-rendered only when its key changes (today's `panelKey`
  logic).
- Opening focuses the heading. The diagram's boxes are `tabindex="0"` with `<title>` tooltips, in
  layer order after the bar's controls.
- **Shipped difference:** the overlay does *not* carry the plan card's per-box "⋯" and side drawer
  yet. Its section drawer ("Sections") shows the ordered list with aim, the known section's status
  ("Known · not checked yet" / "Known · checked ✓", or "Checked: needs teaching after all" after a
  `knownRefuted`) and the current position; plan changes while teaching go through its "Change
  plan" form, the background-planner path the card's edits also use. The card is the surface the
  learner approves on, so the per-section controls live there (owner-requested, 2026-11-07); wiring
  them into a modal that re-renders on `panelKey` — with `edit_plan` now legal in any phase that has
  a plan — is the next step, not part of this change.

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

#### 7.2.12 Read-only sessions

- The head shows back, title, and "Read-only" in `--ink-3` in place of the phase. There is no Plan button.
- The flow renders from the ops, with unanswered exercises showing "Not answered.".
- The desk is in the read-only state (§6.3): the legacy note, or the desktop-plugin note on a phone.
- Focus on open goes to the desk note's button ("Start a new session" or "Continue here anyway") with `preventScroll`.
- A phone session locks after a turn that used a desktop-only plugin: when a `tool` or `artifact` op whose `platforms` lacks `mobile` arrives, the tutor's `turnEnd` triggers one `load_session`, and the session is re-opened if it came back read-only. On Home the list shows such a session as "Locked" (§7.1 rows).
- A figure from a plugin that is installed but doesn't list this device's platform (`PluginInfo.onPlatform` false) shows its fallback SVG; as an answer view it adds "This figure’s plugin doesn’t run on this device." and takes no answer.

#### 7.2.13 Composer: files, paste, drop, `@` mentions

The composer is the same element on Home and in sessions (§9.4). A `composer-attach` block above the field holds the note and chips; the "+" button sits at the start of the field's row.

```
┌ desk ─────────────────────────────────────────────────────────┐
│ [▣ sketch.png ×] [▣ notes.md ×] [▣ circuits-lab/ ×]            │
│ ┌ + ┐ ┌ Reply to the tutor… ──────────────────────┐  [Send]    │
└───────────────────────────────────────────────────────────────┘
```

- **"+" button.** A 44 px button with a visible "+" and accessible name "Add files". It opens the native picker (`pick_files`, multiple selection) and stages each result with `stage_file`: a PNG, JPEG, GIF or WebP file of at most 5 MiB (the bytes decide, not the name) becomes an image chip like a pasted image, anything else a path reference. A file inside the session's workspace is recorded relative to the workspace. Hidden in Practice (§7.3). The workspace chip is in the running head (§7.2.14), not the composer.
- **Paste.** Pasting an image from the clipboard stages it (`stage_image`) and adds a chip. Text paste is never intercepted: when the clipboard also holds text (a copy from a browser or office app often carries an image rendering too), the text pastes as usual and no image is added. An image over 5 MiB is refused with an inline message under the field.
- **Drag and drop.** Dragging files over the composer gives it a dashed `--accent` outline; dropping stages each file. The native Tauri drag-drop handler supplies real paths when available, and its enter/over/leave events drive the outline (HTML5 file drags never reach the page then); a dropped path is staged like a picked one, so a dropped image becomes an image chip. In a browser, a drop with a path is staged the same way, a dropped image without a path is staged as an image, and any other file without a path asks the learner to use "Add files".
- **Chips.** One flexible row above the field, in the order added: a 28 px thumbnail for images, the name, the kind and size in `--fs-meta --ink-3`, and a "×" button whose accessible name is "Remove ⟨name⟩". A folder chip from an `@` mention also has "Use as workspace" (§7.2.14). Pending chips belong to the draft: they survive the composer being hidden and leave only on Send; a rejected send puts them back.
- **Where image bytes live.** A staged image waits in a device-local folder (`<config>/staged-images/`), never in the synced data repo. Send moves it into the repo (`attachments/<yyyy>/<ulid>.<ext>`) as the message op is written. Removing its chip, or "Send without images", deletes the staged copy (`unstage_image`), and the staging folder is emptied at startup, since chips don't survive a restart. Reason: a screenshot the learner thought better of must not be stored and synced.
- **Message text.** A message needs text; chips alone don't send (Send with an empty field does nothing, as before).
- **`@` mentions.** Typing `@` at the start of a word opens a list above the field. The textarea keeps focus and carries `aria-autocomplete="list"`, `aria-controls`, `aria-expanded` and `aria-activedescendant`; the list is a `role="listbox"` of `role="option"` rows, each showing name, kind and path. Up/Down move, Enter or Tab inserts the **path as listed** (`@~/notes/ohms-law.md ` under home, `@week1/ohm.md ` inside the workspace, the absolute path under a granted root) and adds a matching chip, Escape closes and keeps the typed text. While an IME is composing (`isComposing`), the keys are the IME's. The list comes from home, the attached workspace and the session's granted roots, each folder once (a workspace inside home is listed relative to the workspace only; hidden directories skipped, at most 50 rows); it is queried 120 ms after typing stops. Workspace paths are relative so the op log never holds the workspace's absolute path. Off in Practice (§7.3). Reason: the learner points at material; a mention is a reference, not a blanket grant — a path outside home is granted only for that exact path, recorded as an op.
- **Vision warning.** When the Tutor or Practice role reports `vision: false`, each image chip's kind line reads "This model can't see images". There is no separate note above the chips: it repeated the chip (§7.10). Send opens one confirmation dialog: "This model can't see images." / "Send without them?" — "Send without images" (primary) or "Keep editing" (the focused, safe choice). Chips are never dropped silently. Unverified capability shows nothing.
- **Keyboard path.** Tab reaches "+ Add files"; Enter opens the picker; Shift+Tab from "+" reaches a chip's Remove button; removing a chip returns focus to the textarea. In the `@` list, arrows/Enter/Tab/Escape work without leaving the field. Choosing "Attach folder…" or "Detach" in the "More" menu returns focus to "More" before the picker opens (§9.1).
- **Sent messages.** A sent message keeps its chips under the "You" text: a 28 px image thumbnail, then name, kind and size. They are for recognition after a restart, not controls.

#### 7.2.14 Workspace folder

- **Where.** Every session view has a "More" button in the running head (accessible name "More session actions") with `aria-haspopup="menu"`. Its `role="menu"` holds "Full screen" / "Exit full screen" (§7.8) everywhere, plus, in a Learn session that can still receive messages, "Attach folder…" (or "Attach a different folder…" when one is attached) and "Detach" (hidden until attached). Arrow keys walk the items, Escape closes and returns focus to "More". No dialog: the head is already a control surface, and one more modal would be a second place to decide.
- **Head chip.** While attached, a chip in the running head shows the folder's name in `--fs-small`, with `title="Workspace: ⟨name⟩"`. It is status, not a control. The op log carries only the name; the absolute path is device-local.
- **Attaching and detaching.** "Attach folder…" opens the native `pick_folder`; the head status line reads "Choosing a folder…" then "Attaching…", and the announcer says "Workspace attached: ⟨name⟩." Detach removes write access immediately and announces "Workspace detached."; files already written stay. Reason: write access is a permission, not a mode, so the learner alone changes it, in one visible place.
- **A folder chip.** An `@`-mentioned folder in the composer has a "Use as workspace" text button (accessible name "Use as workspace: ⟨name⟩"). It attaches that exact folder without another picker.
- **Another device.** If `workspace_path` returns nothing, the head chip reads "⟨name⟩ (not on this device)" and its title explains that the folder was attached elsewhere; the menu's "Attach folder…" binds a local path to the same session. Nothing else changes.
- **What the learner sees change.** The agent can read the folder (it is a read root) and may write text files (`workspace_write`); the view itself has no file browser. The head chip is the visible sign of that permission.

#### 7.2.15 Artifact figures and views

An artifact reference in tutor prose, `![alt](artifact:<id>)`, becomes one `<figure>` in the page column:

```
┌ figure ─────────────────────────────────────────────┐
│  <the view, or its static SVG>                      │
└─────────────────────────────────────────────────────┘
```

- **Placement.** The figure appears where the reference is, inline in the paragraph (`margin-block: var(--s4)`), inside the prose column. A wide SVG scales down to the column width. No caption: the alt text is the accessible name, and a visible caption would duplicate it.
- **Fallback SVG.** The core's sanitised fallback is visible immediately, sanitised again in the UI with DOMPurify's SVG profile plus the core's rules (`docs/plugins.md` §6.2): `style`, animation elements and fetching CSS functions are dropped, `href`/`xlink:href` keep only `#id` or `data:image/png|jpeg|gif|webp;base64,`, and `url(...)` keeps only `url(#id)`. The SVG has `role="img"` and `aria-label` = the alt text. The figure has a `--hair` border and `--paper` background, so a missing figure is still a bounded box. Theme colour comes from `currentColor`.
- **View.** When the plugin is installed, enabled and declares a view, the figure is replaced by an iframe: `sandbox="allow-scripts"` (**never** `allow-same-origin`), `title` = the alt text, `src` = the shell's `pluginview` URI (`pluginview://localhost/<id>`, or `http://pluginview.localhost/<id>` on Windows/Android), width 100%, height from the view's `resize` message clamped to 160 px–80 % of the window height. The scheme's response carries the view's own CSP (§7.2.17). **The work order's `srcdoc` is not used:** a `srcdoc` frame inherits the app CSP (`script-src 'self'`), which blocks the view's inline script. Measured in headless Chromium 155, not inferred (`docs/plugins.md` §7.2). The host posts `init` with the artifact data, the current CSS custom properties as theme tokens and whether this is an answer surface; a later `theme` message carries new tokens after a light/dark switch. Messages from any other source are ignored.
- **IPC must be unreachable.** The committed probe view (`plugins/fixtures/ipc-probe`) tries 49 ways out from inside a real view frame: the Tauri globals and `window.ipc`/`window.webkit.messageHandlers.ipc`/`window.chrome.webview` in the frame and through `window.parent`, the parent document and storage, `fetch`/XHR/WebSocket/EventSource/beacons to the app origin and `ipc://`, navigation and `eval`. `scripts/ui-check/run.sh ipc` runs it in headless Chromium against the mocked UI and fails on any escape; on 2026-11-07 (Chromium 156) all 49 were blocked and no request left the frame (`docs/plugins.md` §7.2). WebKitGTK and the mobile webviews still need the manual run described there (`docs/agent-system-plan.md`, Known gaps).
- **No view, or it fails.** Missing, removed or disabled plugin, no `[[view]]`, or a failed load: show the fallback SVG. A missing artifact id renders the alt text in brackets in `--ink-3`. The learner is never shown an empty frame. In answer mode, a missing plugin also gets one line explaining that the picture can't take an answer and pointing to Settings → Plugins.
- **Unreferenced artifacts.** Artifacts created in a turn that the reply never referenced are appended after that turn's message in creation order, each in its own figure, so nothing is lost. Reason: a model that forgets a handle should not make the work disappear.
- **Answer mode.** For a pending `question_ask` with `view`, the task line sits above the figure and the view shows its own submit control. After submit, the frame becomes inert (only `resize` is still handled), gets `aria-disabled="true"`, and "Answer submitted." appears under the figure in `--fs-small --ink-3`. The plugin's grade goes to the **tutor only** — its detail can name the target, which W7 says the learner never sees. The tutor still records the verdict.
- **Motion and theme.** A view may not animate unless the learner acts; it honours `prefers-reduced-motion`; it uses the theme tokens passed in `init` and on `theme`, so it reads in light, dark and `forced-colors`.
- **One center.** A view that is not an answer surface is part of the page, not a second place to act (P1): the desk keeps the composer, and moving a slider never changes the desk, takes focus or scrolls.
- **Replay.** Everything comes from the `artifact` op. Opening a session never runs a plugin; an uninstalled plugin still shows the fallback.

#### 7.2.16 Plan-card install chip

- **Installed plugins** stay checkboxes in a section's "Tools" fieldset, all checked; unchecked ones become `removedPlugins`. The label is the installed plugin's title, or the registry's title when only proposed.
- **An uninstalled plugin** named in a section gets one text button instead of a checkbox: "⟨Title⟩ — install" (id when no title is known). It cannot be checked, because installing runs third-party code and needs consent. The chip is offered only when the id is in the section's code-computed `needsInstall` (W9) or the plan predates that field; a stale plan whose missing id isn't proposed shows plain "⟨Title⟩ — not installed" text, never a dead install button. The section also gets the line "Not installed: skipped if approved."
- **The chip** opens the consent screen (§7.5.1). On success the row becomes a checked checkbox without re-announcing the plan; on cancel it stays an install chip; on a hash mismatch the consent dialog shows the error and the chip stays enabled. Installing does not change the plan version and does not grant the plugin: one still missing **at approval** is left out of every section, with the one-line plan notice "Left out until installed: …"; once installed, the tutor can grant it mid-section with `plugin_request`.
- **No registry** has the id: the chip still works, but instead of opening the consent screen it puts "Not in any registry here. Add one in Settings → Registries." in the plan card's status line. The learner is never left with a dead button and no explanation.
- **Approving the plan** installs nothing. This keeps D10's consent rule: the planner proposes, the learner decides.

#### 7.2.17 View document CSP

The app's own CSP (`crates/app/tauri.conf.json`) stays strict: `default-src 'self'; …; script-src 'self'; frame-src pluginview: http://pluginview.localhost`. A view document comes from the `pluginview` scheme with its own header:

```
Content-Security-Policy: sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'
```

`sandbox allow-scripts` keeps the document on an opaque origin even if it is loaded outside an iframe; `connect-src 'none'` blocks every network path. Reason: the view's script must be able to run, but it must not inherit the app's privileges or gain any of its own. Tauri 2.12.5 injects its IPC scripts into the main frame only, and the frame's opaque origin blocks `parent.*` anyway. `scripts/ui-check/run.sh ipc` checks this policy in headless Chromium; WebKitGTK verification is still owed (`docs/agent-system-plan.md`, Known gaps).

#### 7.2.18 Settings → Plugins and Registries

- **Plugins** lists each installed copy: title, version, description, id, enabled state, source, access in plain words ("Access: No files, network or programs." for a Luau api-1 plugin), and its tool names. Each row has Enable/Disable, Remove and, when a configured registry lists a newer version, "Update to ⟨version⟩". A folder plugin whose files failed to reload shows its `loadError` in `--danger` text under the row. If the lock file names a plugin this device doesn't have, a panel above the list ("Missing on this device:") offers "Install ⟨id⟩ ⟨version⟩" (or "Add folder…" for a folder source). Empty states: "No plugins yet." and "No registries yet." The consent screen shows title, description (or "No description."), Version, Source and Access, then the buttons; the access fact appears once, in the Access row.
- **Registries** lists each registry's URL, its last fetch time or error, and the number of plugins in the cached index. The built-in default registry comes first, tagged "default"; removing it says how to bring it back (add its address again). "Add registry" takes a name and address, "Refresh registries" refetches every index. Indexes older than a day are also refetched in the background at startup.
- Both sections use the same buttons, spacing and `role="status"` lines as the rest of Settings. Nothing installs automatically: every install or update first opens the consent screen (§7.5.1), whose primary action is "Install" (or "Add plugin" for a folder) and whose safe action, "Cancel", has focus.
- **Reason.** D9/D10: installing runs third-party code, so the learner sees the version, source and access before agreeing. Update is explicit because a running session must not silently change its tools.

### 7.3 Practice session

Behavior is **unchanged**, except for attachments:

- `send_message({ sessionId, mode: "practice", text, attachments, onEvent })` with a per-call channel;
- `retry_reply` for Retry and "Get a reply";
- `session` events update the list and title;
- Practice never calls Learn commands.
- **Images only.** Practice has no file tools, so its composer hides "+" and turns the `@` list off; pasted and dropped images still become chips, while a dropped non-image file gets "Practice can take images only." Switching the composer to Practice removes pending file and folder chips with the same note. The backend refuses file and folder attachments in Practice, and a file line replayed from an older Practice session omits the "read it with file_read" hint.

Visual changes only:

- The head: back, title, "Practice".
- The page: tutor prose and learner entries as in §6.4. Exercises are whatever the tutor writes in prose.
- The desk: composer, or composer + Stop.
- The streaming scroll rule is identical to Learn (§7.2.2).
- **Reasoning** (`thinking`): rendered *before* the reply text, in reading order with how it was produced, as a closed `details` with summary "Tutor's working". It is never auto-opened, and while streaming the summary does not change text.
- Errors: the block from §7.2.11 with "Retry" → `runPractice(null)`.

### 7.4 Session list and navigation

- The session list exists only on Home (§7.1).
- From a session: "← Sessions" goes Home. Focus lands on the open button of the session just left, or on the input if it was deleted.
- Delete and Extract are available from Home only (the row's "…" menu).
- The open session is not marked "active" in a list, because no list is visible in sessions.
- Practice-busy navigation guard: unchanged rule, with an inline message instead of a toast (§6.2).

### 7.5 Settings

Settings is a modal `<dialog>` that fills the viewport (`inline-size: 100vw; block-size: 100dvh`), with a 640 px content column. It is opened from Home's "Settings" or from an error's "Settings" button. Scrim, no shadow.

```
 Settings                                                     Close
 ─────────────────────────────────────────────────────────────
 Model roles
 Only Tutor is required.

 Tutor (required) · heavy · teaches you        ← fieldset legend: label · weight · phrase
 Guidance (one line, e.g. "Your best model. Learning quality depends on it most.")  ← --fs-small --ink-3
 Usage: in 12,400 tok · out 3,100 tok          ← --fs-meta --ink-3
 Provider [ WorkBuddy        ▾]
 Model    [ gpt-…            ]   (search: top 10 matches, or the provider's first 10 before typing)
 Reasoning effort [ medium ▾]                  ← only when the model lists efforts
 Note: Tool calling unverified.                ← warnings as plain lines, prefix "Note:"
 [Use this model]  Clear  Test  Reload models   Saved — Tutor uses gpt-x   ← status inline, role=status

 ▸ Other roles (optional)                       ← details; each role as above, legend adds "uses the Tutor model" when unset

 ▸ Providers  1 connected                      ← collapsible heading, closed by default
   Keys stay on this device.                    ← body, shown when open
   WorkBuddy        Signed in as …            Manage
   OpenAI           Not connected             Connect
   …

 ▸ Sync  Synced 5 min ago                      ← collapsible; §7.5.2

 Focus
 [x] Full screen in sessions
 [x] Silence notifications in sessions (mako)  ← only when a backend is detected
 [x] Break nudge every 50 min

 Learning
 Starting point for new sessions
 (•) Measure me   ( ) Quick check   ( ) I'm new to this
 How much I check what you already know before planning. You can change it per session.

 Plugins
 Missing on this device: circuit 0.1.0  [Install circuit 0.1.0]   ← only when the lock names one
 Circuits 0.1.0                                ← one row per installed plugin (§7.2.18)
 circuit · Enabled · folder circuit
 Access: No files, network or programs.
 [Disable]  Remove  Update to 0.2.0
 [Add folder plugin…]

 Registries
 Example registry  https://…/index.json  12 plugins, fetched 10:42   Remove
 Name [        ]  Address [                 ]  [Add registry]  Refresh registries

 Appearance
 Theme  ( ) System  ( ) Light  ( ) Dark
```

- Sections are `h2` headings in `--fs-ui` 600, separated by `--hair` with `--s7` spacing.
- Role "cards" become `fieldset`s with no fill. The "required" and "using Tutor model" chips become legend text.
- Statuses that are toasts today ("⟨Role⟩: using ⟨model⟩", errors) appear inline after the button row (`role="status"`, `--danger` for errors) and persist until the next action in that fieldset.
- "Test" results: "Tool call OK" (`--ink`) or the error (`--danger`).
- Provider rows: name, status text (no dot), and a secondary button "Connect", "Manage" or "Close" with `aria-expanded`. The expanded body is inline (keys, base URL `details`, WorkBuddy sign-in). The WorkBuddy waiting state shows "Waiting for browser sign-in…" as static text; the spinner is removed. All existing behavior (autoPick, reload, login polling) is unchanged.
- **Help lines are one short fact each** (§7.10): Model roles "Only Tutor is required."; Providers "Keys stay on this device."; Plugins and Registries have none (their buttons, the consent screen and the row's "Access:" line say it); sign-in hints are "Uses WorkBuddy credits.", "Uses your Claude Pro/Max plan.", "Uses your ChatGPT Plus/Pro plan." and "Paste the code from the browser."; "Reopen sign-in ↗". ChatGPT sign-in waits for the browser redirect like WorkBuddy ("Waiting for browser sign-in…", Reopen, Cancel). Role guidance (`ModelRole::guidance` in `crates/harness/src/config.rs`, quoted in `docs/architecture.md` §4.22) is cut to one line per role. Model warnings: "Not in catalog; capabilities unverified.", "128K+ context advised; unverified.", "Context 64K; 128K+ advised.".
- **Appearance** is new and UI-only: a radio group bound to `localStorage["lh.theme"]`.
- **Learning** is new (§7.7.7): one radio group, "Starting point for new sessions" (Measure me / Quick check / I'm new to this), read from `get_settings().startingPoint` and written with `set_starting_point({ value })`. Each option carries one help line under it; a `<p class="status" role="status">` beside the group says "Saving…" → "Saved." or the error, and the stored choice is put back on failure. It sits between Model roles and Focus.
- **Focus** is UI-only and sits right after Model roles: three checkboxes bound to `localStorage["lh.focus.fullscreen" | "lh.focus.dnd" | "lh.focus.breaks"]` (`"on"`/`"off"`, default on). No help text: the labels say what each does (§7.8).
- **Collapsible sections:** Providers and Sync are closed by default, to keep the dialog short. Their `h2` holds one full-width button (`aria-expanded`, `aria-controls` the body) with a chevron drawn in `--ink-2` (right when closed, down when open; no motion, §8), the section name and a one-line summary in `--fs-small` `--ink-3`. The summary shows whether the section is open or closed and updates with the data: Providers "⟨n⟩ connected" / "None connected"; Sync the Home sync icon's status ("Synced 5 min ago", "Not synced", "Syncing…"), or "Off" when no target is set (nothing when `sync_status` fails). Enter and Space toggle it (native button). The learner's own open/close is kept per device in `localStorage["lh.settings.providers" | "lh.settings.sync"]` (`"open"`/`"closed"`) and put back on every Settings open (`ui/disclosure.js`). Navigating into a section opens it for that visit only and leaves the stored choice alone: the sync panel's "Sync settings" and the banner's "Review" open Sync (§7.5.2); Home's "Connect a model to start → Settings" opens Providers; every "Choose a model first" path and a model error's "Settings" button open Providers when no provider is connected (Model roles then says "Connect a provider below"). Focus lands on the section's heading button (or the needs-attention heading), never inside a closed body. A waiting sign-in (WorkBuddy, Claude, ChatGPT) keeps running while Providers is collapsed: its rows are only hidden, and the result shows when the section opens again.
- **Sync** sits right after Providers; see §7.5.2.
- Phone: the same single column; button rows wrap.

#### 7.5.1 Consent screen

Every install, update, reinstall from the lock and folder addition opens the same modal `<dialog>` (`#consent`, `aria-labelledby` its heading) before anything runs; nothing else in the app installs.

```
 Circuits                                       ← h2: the plugin title ("Install this plugin?" when unknown)
 Draw, solve and explore DC and AC circuits.    ← description, or "No description."
 Version   0.1.0                                ← "declared by the folder" for a folder plugin
 Source    https://example.org/index.json       ← registry URL, or the folder path
 Access    No files, network or programs.       ← permissions in plain words
 [Install]  Cancel                              ← "Add plugin" for a folder; Cancel has focus
 Installing…                                    ← role=status; then "Installed." or the error
```

- **Access** is the only place the permissions appear. A Luau api-1 plugin has none, so it reads "No files, network or programs."; a native plugin lists each permission in words ("Run programs on your device", "Use the network", "Read your files", "Write your files").
- **Focus** starts on Cancel, so Enter never installs by accident. Escape, Cancel and a click on the scrim close it without installing.
- **While installing** both buttons are disabled and the status reads "Installing…". On failure (hash mismatch, invalid package, a section using the plugin) the error stays in the status line in `--danger`, both buttons come back and focus returns to Install; the dialog does not close. On success it closes and the caller's row updates (plan chip → checkbox, Settings row, missing panel); from a plan chip the announcer also says "⟨Title⟩ installed."
- **No consent screen without a candidate:** a plan chip whose id no cached registry lists reports that in the plan card instead (§7.2.16).
- **Reason.** D9/D10: installing runs third-party code, so the learner sees what it is, where it comes from and what it may touch, and agrees in one place that is the same everywhere. Focus on the safe action matches the other confirmations (§7.2.13's image confirmation focuses "Keep editing").

#### 7.5.2 Sync

The **Sync** section sits right after Providers (`#sync-section`, `ui/sync.js`). It is collapsible like Providers (§7.5): closed by default, its heading reads "Sync" plus the Home sync icon's status (or "Off"). Help line, inside the body: "Settings stay on this device."

```
 ▾ Sync  Synced 5 min ago                      ← heading button (open)
 Settings stay on this device.
 Git          Last synced 5 min ago · 2 ahead      ← h3 + state line (--fs-small --ink-3)
 the token was rejected; …                       ← lastError, --danger, only when set
 Remote URL   [https://github.com/you/notes.git]
 Access token [Saved · leave empty to keep]       ← password field; placeholder only when hasToken
 Branch       [main]
 [Save]  Disconnect                              ← Disconnect only when configured
 WebDAV       Not connected
 URL [https://…]  HTTPS only, or a localhost test server.
 Username [ ]  App password [ ]
 [Save]
 Needs attention                                 ← only when needsAttention is not empty
 The other device's copy is kept in .conflicts/; this device's version was kept.
 • graph/topics.json                             ← monospace
 [Acknowledge]
 [Sync now]   Synced. Git: 1 pulled, 2 pushed. WebDAV: 3 uploaded, 1 downloaded.
```

- **Two targets, one form each** (`set_sync { remote, token, branch }`, `set_webdav { url, username, password }`). Each has its state line ("Not connected", "Not synced yet", or "Last synced ⟨relative⟩", then "⟨n⟩ ahead", "⟨n⟩ behind" or "changes waiting"), its last error, an inline status ("Saving…" → "Saved." or the backend's sentence, e.g. a plain-http WebDAV URL) and Disconnect (`remote: ''` / `url: ''`; no confirmation, like removing a provider key). Focus moves to the target's first field when Disconnect hides.
- **Secrets** are never shown. A saved token or app password shows as the placeholder "Saved · leave empty to keep"; an empty field sends `null`, which keeps it. The fields are filled from `sync_status` on each Settings open.
- **Sync now** (secondary; enabled when a target is configured) reads "Syncing…" and is disabled while it runs, then re-reads `sync_status` so each target's error shows on its own line. The section status gives the report ("Synced. Git: … WebDAV: …") or, after a failure, the same per-target summary as the panel ("Git didn't sync (see above). WebDAV synced.").
- **Needs attention** lists the parked paths with one "Acknowledge" (`acknowledge_conflicts`); afterwards the status says "Acknowledged." and focus moves to the Sync heading button. "Review" in the Home banner expands Sync and focuses the "Needs attention" heading.

### 7.6 Learning map (its own page)

The Map draws the **knowledge graph**: a single-home topic tree whose leaves hold propositions
(one claim each, with a description), joined by prerequisite edges. It is based on knowledge, not on
sessions — no prompt text, plan goal, phase or "in progress" appears on it, and a session can only
ever *highlight* part of it (architecture §4.2, §4.5). The store is `<repo>/graph/` (`topics.json`
tree, `nodes.jsonl` propositions, `edges.jsonl` prerequisites, `checks.jsonl` per-topic totals),
separate from the session op logs, so the map's structure never depends on which sessions exist. It
is written by the graph builder from live sessions as they go (architecture §4.7: after a tutor turn once
enough new teaching has accumulated, at `session_end`, and from a session's Extract on Home), and was
seeded once by `cargo run -p harness --bin import_learnharness -- <learnharness-data> <repo>`.

**Where it lives, and why there.** A separate page reached only from Home ("Map" in the top bar).
It is never Home's background (LearnHarness draws its graph behind the start input) and never
inside a session. A drawing in the periphery of the start question or the lesson is visual clutter
and a seductive detail (§2.1, §2.3; [Rosenholtz 2007; Sundararajan & Adesope 2020]); choosing what
to practise is a deliberate, separate task, which is when an open learner model helps most
[Mitrovic & Martin 2007; Long & Aleven 2017].

**Why it may be flashy (owner decision, 2026-11-06).** The Map is where the learner sees what
their hours bought; it's motivational, not a reading surface. Two findings shape the flash:

- Juicy (abundant, immediate) feedback motivated players through *competence* when it was
  **success-dependent**; amplification for its own sake *lowered* effectance, competence and
  curiosity, plausibly by blurring the action–outcome link (pre-registered, n = 1 699
  [Kao 2024]). Visual juiciness raised aesthetic appeal without changing usability or
  performance [Hicks 2019, as summarised in Kao 2024].
- Perceived progress toward a goal increases effort [Kivetz 2006] (§2.7).

So every effect on the Map answers the learner's hand and nothing glows for decoration. What
remains is the opening burst, the click sparks and rings, and the HUD count-up. Sessions keep the
Carrel motion rules (§8); the Map is the one exempt page, and the learner reaches it only on
purpose, from Home.

**No edge motion, and no per-claim state (owner decisions, 2026-11-07).** The light particles that
travelled along lit edges are removed; so are the ignite sequence, the needs-work breathing halo,
the selection ring's rotating dash and the star twinkle. Two reasons: an animation that runs
without the learner's hand is exactly what §2.1's onset rule forbids, and the graph has **no
per-claim check state** to light — the importer records per-*topic* quiz totals only, so a glowing
claim would be an invented fact. Claims are therefore styled by **kind**, and the evidence is
stated in words on the cluster it belongs to.

```
 ← Home   Learning map                                            (running head)
┌──────────────────────── dark stage, full width ──────────────────────────┐
│ 49 ideas · 9 topics · 35 recorded prerequisites                            Fit    │  (HUD; counts up once on open)
│                                                                          │
│              Mathematics › Calculus › Limits and continuity              │  (cluster title, serif, screen space)
│                      34 answers · 11 wrong                               │  (the topic's real quiz totals)
│                 ○          ●          ○        ○                         │  (claims; size = how many need it)
│                    ╲      ╱   ╲    ╱                                     │  (prerequisite arrows; static)
│              ○      ○─◐─○      ○─○──○    ○                               │
│                 ╲  ╱                                                     │
│              Mathematics › Calculus › Exponential and logarithmic rules  │
│                      3 answers · 0 wrong                                 │
└──────────────────────────────────────────────────────────────────────────┘
 3 selected  Clear                                   [Practise 3 ideas]   (desk)
```

**Stage.** The page column gives way to a canvas stage the full width and height between head and
desk. The stage is dark in both themes: light only reads as light on dark. Label ink on the stage,
measured against its lightest point `#15221D`: `#F0ECE4` 13.9:1 for claim labels, 62 % of that ink
6.1:1 for claims not selected. A sparse star field drifts at a fifth of the pan (parallax) but does
not twinkle.

**Clusters are topics.** `knowledge_map()` returns a flat `nodes` list; the UI groups it by each
claim's `home` topic and builds a cluster per topic that owns at least one claim. The cluster title
is the full path walked up `parent` — `Mathematics › Calculus › Limits and continuity` — because a
leaf name alone ("Series and Taylor") does not say which field it belongs to. When
`topicChecks[home]` exists, its real totals go in the subtitle (`34 answers · 11 wrong`); when it
does not, the subtitle is omitted rather than estimated. On the current data: 9 clusters over 49
claims, 4 of them with check totals.

**Edges are global.** There is one `edges` list (`from` is a prerequisite of `to`), not one per
cluster: 35 edges here, with 13 claims having two or three prerequisites and 4 edges crossing
topics. Cross-topic and multi-parent edges are the point of the graph, so the physics receives the
whole list by claim id; only the in-cluster subset feeds the layered "home" positions. Arrows are
static (§8 exemption above).

**The graph is as dense as the extractor made it, and the UI says so.** The importer only carries
the source's own chains — 25 chains, 39 premise references, 35 distinct edges over 49 claims (one source chain closed a 2-cycle; the importer now drops the later direction — see below) — so
**29 of the 49 claims have no recorded prerequisite**. Many of those genuinely are foundations
(`Epsilon–delta definition of a limit`, `Function composition`, `Definite integral`); others are not
(`Logarithm of a sum vs. logarithm equation` sits next to `Logarithm as inverse exponential` in the
source and still has no incoming edge). The missing relation is an extraction gap (the graph builder
job, arch §4.7, is not built), not a drawing gap, so the UI never fills it in:

- the HUD counts **recorded** prerequisites, never "prerequisites";
- a claim with no incoming edge reads `No prerequisite recorded` (and `Nothing recorded depends on
  this` for the outgoing side), not `Depends on (0)` — "depends on nothing" would be a claim the
  store cannot make;
- the visually hidden list says `No prerequisite recorded.` for the same reason.

Reason: an open learner model is only useful if its blanks are visible; inferring prerequisites from
statement wording would put edges in the graph that nobody verified, and the next session would
teach against them.

**Proposed links are reviewed, not applied.** `extract_edges` writes what a model thinks the graph is missing to `graph/edges.proposed.jsonl` with a `why` per edge; the Map shows a quiet `Review N proposed links` button next to Fit whenever that file has rows, and a modal list behind it:

```
 Proposed links                                                    Close
 Each row is an edge the extractor proposed, with its reason. Accept
 adds it to the map; nothing here is added until you accept it.
 ─────────────────────────────────────────────────────────────────────
 Epsilon–delta definition of a limit → Fixed-radius constancy requirement
 Prerequisite of
 The fixed-radius argument is built from the definition's condition that
 all x with 0 < |x − a| < δ satisfy |f(x) − L| < ε.
 [Accept]  [Dismiss]
 ─────────────────────────────────────────────────────────────────────
 ...
 [Accept all]                                        3 added, 0 refused.
```

- Rows show **titles, never ids** (the command sends a `titles` map), the relation in words ("Prerequisite of" / "Easily confused with"), and the extractor's own sentence verbatim — the reason is the thing being judged, so it is not summarised or rewritten.
- Decide per row or `Accept all`; decisions are saved when the list closes (one command, one write), the dialog is emptied, the Map reloads, and the result appears in the page's own status line ("3 links added to the map; 1 refused: would close a prerequisite loop."). Accepting is the learner's act; nothing is applied behind their back.
- **The core re-validates**: unknown ids, self-edges, duplicates and anything that would close a cycle are refused at accept time and reported with the reason — the extractor's file can be wrong without the graph becoming wrong. Refused rows leave the file too, so they are not asked about twice.
- An empty file has no button and no dialog: a Map with nothing to review looks exactly like one without proposals.


**Claims are styled by kind** (`mechanism`, `fact`, `procedure`, `perceptual`, `argument`), which
the graph stores per claim: ink colour, ring weight and ring dash differ per kind, and the kind is
also a word in the hover card, so colour is never the only carrier (WCAG 1.4.1) [Haroz & Whitney
2012]. There is no checked/needs-work/not-yet distinction, because no per-claim verdict exists.

**Layout and physics** (`ui/map-physics.js` is pure math; `ui/map-layout.js` gives each claim its
home position):

- Within a cluster every claim has a **home**: prerequisites above, dependents below — the layered
  arrangement that keeps crossings low [Purchase 1997; Sugiyama 1981; Gansner 1993].
- **Topics nest; subjects are islands** (2026-11-11). Each cluster sits inside its topic's block,
  and a block holds its own ideas' cluster plus its sub-topics' blocks, packed in shelves to about
  1.6 : 1 (gaps 260/220 inside a sub-topic, 360/300 between a root's own blocks). Root topics
  (Mathematics, History, …) are separate blocks with **1100 units of water** between them, so two
  unrelated subjects never read as one; roots that share a prerequisite are placed next to each
  other (greedy order by shared edges, ties keep the topic-tree order) with a 640-unit strait. A
  block that only holds sub-topics gets a 160-unit band for its heading. Reason: the old single
  shelf row put `History › Rome` straight after `Mathematics › Geometry` and the eye merged them.
- A d3-style simulation (forces scale with a cooling `alpha`) adds charge between claims, springs
  along prerequisite edges, disc collision, a pull toward home (firm in y, loose in x) and a hard
  rule that whatever needs a claim sits below it. A spring across clusters is weak (0.1 within one
  root, 0.02 across roots) and the "sits below" rule applies inside a cluster only: across islands
  both would drag claims out of their topic into the water.
- **Stiff, not elastic** (owner direction, 2026-11-07). Velocity damping 0.38 → 0.55, cooling
  0.018 → 0.035, home pull roughly doubled (x 0.07 → 0.16, y 0.11 → 0.22), springs 0.6 → 1.0, and
  a flung claim is capped at 20 units/tick instead of 40. Measured on the real graph: rest in
  **175** ticks instead of 343 (~3 s instead of ~6), mean distance from the layered home slot 144
  instead of 174, and a flung claim overshoots 14.7 units instead of 28.5. Reason: a map is a
  reference surface, not a toy — oscillation reads as uncertainty about the facts, and every
  settling frame is a frame the learner waits through.
- **Opening:** claims start at their cluster's centre and burst out over about a second, capped at
  48 units a tick. The settled picture is the same every visit (mental-map stability [Misue 1995]).
- A claim's disc grows with how many claims need it (radius 12 + 3 per dependent, up to 4), so the
  load-bearing foundations are visible before any label is read.
- **Fit frames the whole graph** (every root block). Framing a single cluster hid the rest of the
  map; at whole-graph zoom the open topics' titles and the folded topics' discs say what is where
  (semantic zoom, below), and the learner zooms into a topic to read its claims.

**Semantic zoom (level of detail, 2026-11-11).** Zooming out folds topics into their parents, so
the far view is a handful of subject discs instead of a dust of dots (verified in headless
Chromium 156 on a synthetic graph — Mathematics › Algebra › Linear equations / Quadratics,
Mathematics › Geometry, History › Rome / WWII, Physics › Mechanics, 41 claims — at the root-disc,
sub-topic-disc, fit and claim zooms; ctrl+wheel over canvas, HUD and buttons was
`defaultPrevented` with `devicePixelRatio` and `visualViewport.scale` unchanged, and a CDP
two-finger touch pinch zoomed the map only):

- A topic is **open** (its claims and sub-topics drawn) while its block is at least **220 px**
  across on screen and folds into **one disc** under **180 px** (the gap stops a zoom resting on
  the threshold from flickering); at zoom ≥ 0.5, where claim labels start, everything is open.
  The rule is by on-screen size, not depth, so a big topic stays open further out than a small
  one and a deep tree needs no extra settings. Zooming out from claims: Linear equations folds,
  then Algebra, then only `Mathematics`, `History`, `Physics` remain.
- A folded topic's disc sits at the mean of its claims, is 15 + 4.5·√n px (max 54, never bigger
  than its block), carries the **number of claims inside**, and is named under it with
  "n ideas · m topics". The name is dropped rather than overprinted (it may not cover a title, a
  claim, another name or disc); the disc's tooltip still gives the full path. A disc holding a
  selected claim gets the dashed selection ring.
- The switch **fades** over ~150 ms (contents and disc cross-fade; instant with reduced motion);
  only things more than half there take the pointer.
- **Edges aggregate.** A prerequisite with an end inside a folded topic is counted toward one link
  between the two things actually drawn (a claim or the outermost folded topic), deduplicated per
  pair and direction, drawn in screen px as a gentle arc (so A → B and B → A stay two lines),
  thicker with the count (1.2 + 1.1·log₂ n px, max 5). Edges inside one folded topic disappear
  with it.
- **Click a folded disc** to zoom into that topic (it opens on the way in). **Click an open
  topic's heading** (a topic that only holds sub-topics) to frame it; a cluster title still
  selects its claims.
- The zoom floor follows the map: 0.3 for a small map, further out (never below 0.02) when needed,
  always far enough that every root folds — fully zoomed out *is* the subjects view.

**Zoom belongs to the map, never the app (2026-11-11).** Before, a trackpad pinch over the Map
magnified the whole window on Linux, and ctrl+wheel over the HUD or the card zoomed the page in a
browser. Now:

- **Wheel** over the canvas zooms the map about the pointer (plain wheel is the map's convention,
  as before). **Ctrl/⌘+wheel** — which is also how Chromium and WebKit deliver a trackpad pinch —
  is caught on the whole stage (HUD, card and buttons included) by a non-passive listener,
  `preventDefault`ed and turned into a map zoom about the pointer, with pinch-sized deltas scaled
  up. A plain wheel over the card still scrolls its long statement.
- **Touch pinch** (two pointers; the stage is `touch-action: none`) zooms about the fingers'
  midpoint and pans with it. Safari/WKWebView `gesturestart/change/end` are cancelled and zoom
  the map about the pointer.
- **WebKitGTK (Linux).** A touchpad pinch never reaches the page there: WebKit's own
  `GtkGestureZoom` magnifies the page first, and no setting turns it off. The app shell
  (`crates/app/src/pinch.rs`) connects to the webview widget's GTK `event` signal, which GTK 3 runs
  before bubble-phase gestures, stops every `GDK_TOUCHPAD_PINCH`, and forwards it as an
  `app-pinch` window event `{ phase, scale, x, y }`. The Map zooms when such a pinch begins over
  the stage; elsewhere a pinch does nothing. **Decision:** this disables WebKit's whole-app pinch
  magnification on every page, not only the Map — it was the cause of the bug, the app's text
  scales with the system font size (rem units, WCAG 1.4.4), and a magnified webview has no way
  back but pinching out again. Tauri's `zoomHotkeysEnabled` is left off (its default), so the
  zoom keys never zoomed the app either. Touch-screen pinches are touches, not touchpad events,
  and are unaffected.
- **Buttons and keys.** A bottom-right group holds `−` / `+` (a smooth 1.4× step about the
  centre) and, when the newest Learn session's plan has claims on the map, **"Last session"**,
  which frames the clusters holding them. With focus in the stage (the idea list, the buttons),
  `+`/`=` and `-` step the zoom and `0` fits, with or without ctrl/⌘.
- **Double-click** a topic (open block or folded disc) to frame it; again to step out to its
  parent; on open water, or from a root, to frame the whole map.

**Cluster titles are screen-space labels.** Drawn at a fixed 16px/13px in the canvas rather than in
world units, so a zoomed-out view still says which topic sits where; each is pinned inside the
stage and placed above its cluster, else one title-height further up, and never below it: a title
reads as the heading of the ideas under it, and one under its cluster looked like a topic with no
ideas (owner report, 2026-11-10). It is **dropped rather than overprinted** when it would collide
with another title, another topic's claim or a folded disc. Zooming in separates the clusters and
the dropped titles return. Only open topics have titles; a block wholly beside the stage has none
(pinning a far island's title to the edge named a topic that wasn't there).

**Interaction.**

- **Click a claim** to select or unselect it for practice: shockwave plus spark burst. **Click a
  cluster title** to select every claim in that topic.
- **Drag a claim**: its neighbours follow on their springs; let go mid-motion and it's flung, then
  pulled home. **Drag the background** (or a folded disc) to pan; **wheel or pinch** to zoom
  (see "Zoom belongs to the map"); press "Fit" to frame the whole graph.
- **Hover or keyboard focus** lifts the claim, dims everything that is not it or a direct
  neighbour, and shows a **description card**: the claim's title; kind and topic path; the
  proposition's own `statement` as the description; then two hoverable labels, `Depends on (n)` and
  `Leads to (m)` (with no recorded edge they read `No prerequisite recorded` /
  `Nothing recorded depends on this`, see below). When the claim's topic has venues the card ends
  with the `Where the field lives` section (its own plus its descendants', grouped Ask / Discussion
  / Reference) — the same topics the HUD's guide lists, reachable from the idea itself.
  - Hovering or focusing either label opens a **second box beside the card** listing those claims'
  titles one per row (no truncation, no single-line summary). It is placed to the right of the card,
  then to the left, then below, and clamped to the stage, so it never leaves the surface.
  - The card is pointer-interactive (`pointer-events: auto`) and hides on a ~150 ms timer cancelled
    whenever the pointer enters the card or the flyout — otherwise the pointer could never reach
    the labels. Both labels are focusable buttons, so the same flyout opens from the keyboard, and
    moving focus between them keeps it open.
- **Claim labels** on the canvas appear at zoom ≥ 0.55 and are laid out greedily: the focused claim
  first, then selected ones, then the rest in graph order, skipping any label whose box would
  overlap one already placed or a topic title or disc name (compared in screen px; the card still
  gives every title in full). Without this, a 22-claim topic printed its titles on top of each other.

**Accessibility.** The canvas is `aria-hidden`. Under it is a visually hidden list: one `fieldset`
per topic (legend: the topic path), then one native checkbox per claim labelled "⟨title⟩,
⟨kind⟩" and `aria-describedby` its depends-on list, followed by the claim's two label buttons.
Tab walks the list; Space toggles; focusing a claim draws its ring on the canvas, shows its card
and pans to it when it is off-screen; focusing a label opens the flyout. Keyboard focus beats mouse
hover. Choosing the checkbox is the same act as clicking the claim, so the two never disagree.

**Desk.** One state: "n selected", a "Clear" text button, and the one primary button, "Practise n
ideas" (disabled, reading "Practise", with nothing selected). Pressing it starts a Practice
session whose first message lists the selected claims with their topics and asks for them mixed,
not one at a time [Rohrer & Taylor 2007; Brunmair & Richter 2019]. Its first line doubles as the
session title until the generated title arrives:

```
Practise: Chain rule, Quotient rule

Mix them together rather than one at a time.
- Chain rule (in “Mathematics › Calculus › Exponential and logarithmic rules”)
- Quotient rule (in “Mathematics › Calculus › Exponential and logarithmic rules”)
```

No "from my Learn sessions" line: the map is knowledge, not a session (owner decision). Without a
model the desk shows "Choose a model first." in `--danger` under the button and opens Settings. The
selection survives leaving the Map for Home and coming back; it is cleared once a practice session
starts.

**States.** Loading: "Loading…". Empty: "Nothing in the knowledge map yet." Failure: "Couldn't load
the map: ⟨message⟩" with a "Try again" text button. The page is loaded once per visit and does not
update while open (P3). A graph whose `topicChecks` lacks a topic, or whose edges name unknown
claims, still renders: subtitles are omitted and dangling edges dropped.

**Reduced motion and forced colours.** With `prefers-reduced-motion: reduce` the simulation runs to
rest before the first paint (no burst, sparks, ring or count-up) and the canvas redraws only on
interaction; dragging moves a claim without inertia and it stays where it's dropped. With
`forced-colors: active` the stage draws in `Canvas` / `CanvasText` / `Highlight` with no glow, and
motion is off as above.

**Focus.** Opening focuses the page heading (`tabindex="-1"`); "← Home" returns focus to Home's
"Map" button.

**Backend.** One read-only command, `knowledge_map()`, returns

```
{ "topics":     [{ "id", "title", "parent": null | "id" }],
  "nodes":      [{ "id", "title", "statement", "kind", "home", "correct": 0, "missed": 0 }],
  "edges":      [{ "from", "to" }],
  "topicChecks": { "<topicId>": { "correct", "missed" } },
  "venues":     [{ "topicId", "kind", "name", "url"?, "entryLevel", "note"?, "checked"? }],
  "topicVenues": { "<topicId>": [ <venue> ] } }
```

`from` is a prerequisite of `to`. `correct`/`missed` on a node are always 0 today: the source
recorded verdicts per *topic*, so the honest place for them is `topicChecks`, and the UI never
derives a per-claim state from them. `venues` is one row per venue, and each row's `topicId` is the
topic whose material it serves; `topicVenues` is the read model the UI uses — every topic's own
venues **plus its descendants'**, so `Mathematics` also carries what was recorded under
`Mathematics › Calculus`. Both are sorted ask → discussion → reference, then by name. The graph
builder is their only writer today (the `venue` op in `graph/ops.jsonl`). The command reads
`<repo>/graph/` and nothing else — no session replay, no cache of open sessions to disturb.

**Where the field lives is on the map (`ui/map.js`).** Because a venue belongs to a topic, the Map
shows it in two places, both from `topicVenues` and never from a session:

- a closed `Where the field lives · ⟨n⟩ topics` disclosure in the HUD, one row per topic that has a
  venue — the full path and the count — and each row's button jumps/zooms the map to that topic's
  cluster. Only topics with a cluster (an idea on the map) can be jumped to, so a topic with venues
  but no claims is not a row; its venues still appear on its parent's row and card;
- a `Where the field lives` section on an idea's hover card, listing the clusters' topic venues
  grouped Ask / Discussion / Reference, each as a name, a link and its note.

Nothing venue-related renders when the graph has no venues, and a Map whose `knowledge_map` is
empty (no topics, nodes or venues) shows none of it even when the newest session's brief still
carries venues: the brief's venues belong to the close (§7.7.5), not to the Map.

**Performance.** One IPC call per visit. One canvas, device-pixel-ratio capped at 2; glows are
pre-rendered sprites stamped with `lighter` compositing (no per-frame `shadowBlur`). The frame loop
runs only while the Map is shown and the window is visible, and stops entirely once the picture is
still — with edges static, a settled map costs no frames at all. Charge is O(n²), fine for the few
hundred claims a studied subject holds.

**Learning-system additions (built; §7.7.6):** the reached and aimed level in the card's detail
line, a confidence line and a "Saved for later" frontier list from the newest Learn session's
`session_state` plus `frontier`, in one block under the counts. The guide is not in that block: it
comes from `topicVenues` (above), so it exists even when no Learn session does.

### 7.7 Learning-system surfaces (built)

These surfaces present the features in `docs/learning-system.md`. That document says *what* and *why*; this section only says how they look, under the Carrel rules:
- one primary action;
- no badges or pills;
- native controls;
- nothing moves on its own;
- extra controls hidden until asked for.

Where these rules and a learning feature conflict, the feature wins and this section changes.

#### 7.7.1 Plan card boxes: the "⋯" inside a box and its side drawer

Owner direction, 2026-11-07. The card is the flowchart (§7.2.9), so **every section box carries a
"⋯" inside its own bounds** and its own state in words under its title. The "⋯" opens one drawer
pinned to the side of the window with that section's long description and every action it used to
have in the list.

```
 PROPOSED PLAN                                                    Expand
 Understand voltage dividers                                  ⋯   ← plan-level options (Depth/Explore/Sitting/Style)
 You already use Ohm's law fluently …

 ┌ plan-figure ────────────────────────────────────────────────────────────┐
 │  ╭ Series current ⋯ ╮        ╭ Divider rule ⋯ ╮                          │  ← the "⋯" sits *inside* the box
 │  │ 1 idea · aim: Use │───────▶│ 2 ideas · You know this · aim: Use       │  ← badges: the section's state
 │  ╰───────────────────╯        ╰──────────────────────────────────────────╯
 │  One box per topic; an arrow joins two topics wherever an idea needs …    │
 └──────────────────────────────────────────────────────────────────────────┘

 [Approve plan]   Send changes   Discard   Suggest a change
```

The drawer — fixed to the inline end of the window (a bottom sheet under 600 px), no scrim, the
plan still readable beside it:

```
┌ Series current                                          Close ┐   ← h3 is the section; it labels the drawer
│ Guided · Socratic · Aim: Use                                  │   ← mode · style, --fs-small --ink-3
│ Goal: explains why the current is the same everywhere in one path │ ← the exit criteria
│ Covers: One current (mechanism), Source resistance (fact)     │   ← its nodes, with their kinds
│ Start from a torch bulb; keep the water analogy out of it.    │   ← the planner's notes (prose)
│ You said you know this. One quick check when we get here.     │   ← known sections only
│ Tools  [x] graph  [x] echo          circuit — install         │   ← native checkboxes / the install chip
│ How far   ○Explain ○Use ○Analyze ○Design                      │
│ Include   ○Keep ○Don't need this                              │
│ Parts I already know…   [textarea]                            │
│ ☐ Hunt — ⟨the planner's one-line reason⟩                      │   ← only when the planner marked one
│ Note for the planner    [textarea]                            │
└───────────────────────────────────────────────────────────────┘
```

- **The "⋯" lives in the box.** It is drawn by `ui/plan-diagram.js` as a `<g role="button"
  tabindex="0">` in the box's top-right corner, with `aria-label` "Details and options for ⟨section
  title⟩", `aria-expanded` and `aria-controls` pointing at the drawer. It opens on click and on
  Enter/Space. Because the drawing then contains controls, its root is `role="group"` rather than
  `role="img"` — an image role would hide the "⋯" from assistive tech. The title is centred in what
  the corner leaves free, so the two never overlap.
- **One drawer, refilled per section.** Pinned with `position: fixed` to the side of the window, it
  is not a dialog: no scrim, no focus trap, the plan stays visible and usable. It shows, in order,
  the section's title, `mode · style · Aim: ⟨level⟩`, its exit criteria ("Goal: …"), its nodes with
  their kinds ("Covers: …"), the planner's notes for the section (rendered as prose), the known
  section's line, "Desktop only", then the actions — the same controls the old inline disclosure
  had.
- **How it closes:** Escape (from anywhere in the card), the drawer's "Close" control, activating
  the same box's "⋯" again, or a different box's "⋯" — which swaps the drawer to that section
  rather than closing. Focus moves to the drawer's heading when it opens and returns to the box's
  "⋯" when it closes; Tab walks in and out of it like any other panel (§9.1) — nothing traps focus.
  A card rebuilt under an open drawer (a state refresh, a plan revision) keeps the drawer open on
  the same section: `s.planEdits.drawer` remembers it, and a new `planVersion` clears it.
- **State at a glance, on the box** (`plan-diagram.js::annotate`): the section teaching *is* is
  outlined in the gold mark; a finished one is shaded and says "Done"; a known section is greyed
  and says "You know this" (its tooltip says "you said you know this"); a refuted known section is
  back to normal. Alongside them, in `--fs-meta` words under the title: `aim: ⟨Level⟩`, or the
  learner's own pending choice (`⟨Level⟩ only`, "Not needed"), plus "Hunt on" and "Needs install"
  when they apply. Nothing is "Current" while the plan is still a proposal. Colour is never the
  only signal: every mark is also a word, and the drawer repeats the facts in sentences.
- **The plan-level "⋯" stays beside the title** ("Plan options"): Depth (Just pass / Solid / Deep),
  Explore (Off / Suggest / On), Sitting length (minutes), Style ("Let me reason it out" / "Explain
  it to me"). It is still an inline disclosure, not a dialog.
- **Actions in the drawer are the old ones, unchanged in meaning:** How far (the four levels, each
  with a one-line description), Include (Keep / Don't need this), "Parts I already know…", Tools
  (checkboxes; the install chip for a `needsInstall` plugin), Hunt (only when the planner marked one
  available, with its reason), and Note for the planner.
- A planner **warning** about a choice (e.g. a level below the goal) appears as one sentence in the
  approach text, not as an alert.

**Edits are local until sent.** Every action writes into the same pending-edit object; while
anything is pending:

- the primary button reads **"Send changes"**, with an optional "Comment for the planner" textarea
  revealed above it;
- "Discard" is a text button;
- **Approve is visible but disabled** (`aria-describedby` the status line) — sending one
  `edit_plan` is the only thing that resolves the pending state, and Discard re-enables Approve;
- Sending writes exactly one `edit_plan` call; the status reads "Sent. Revising the plan…"

**Shipped (2026-11-07, `ui/cards.js::planCard` + `ui/plan-diagram.js`).** Every point above is in,
with these details:
- State lives in one object (`newPlanEdits()`): `{ open: Set, drawer: null, plan: {}, sections: {},
  comment: '', answered: null, hunts: {} }`. A change redraws the affected box's badges and the
  action row in place; nothing re-renders the card or empties the drawer (that would drop the caret
  in a textarea). Only radio/checkbox changes redraw the drawing; typing in a textarea does not. A
  new `planVersion` clears the object.
- Values sent, in the vocabulary the backend stores: Depth `pass | solid | deep`; Explore `off |
  suggest | on`; Style `socratic` ("Let me reason it out") / `expository` ("Explain it to me"); How
  far `explain | use | analyze | design`; Include `keep: true | false`. Cleared fields are omitted,
  not sent as null.
- The payload is `{ depth?, explore?, minutesPerSitting?, style?, sections?: [{ sectionId, targetLevel?, keep?, knownNote?, learnerNote?, plugins? }] }` — plan-level fields at the top, one entry per edited section, every unedited field omitted (the backend refuses an empty object, so a comment alone never opens "Send changes"). `plugins` is the list the learner kept, and only present when they touched the Tools checkboxes. `sectionId` is what `agent.rs::describe_plan_edits` reads; it also accepts `id`.
- **The hunt toggle is immediate**: it calls `set_hunt({ sessionId, sectionId, enabled })` (one op, no planner job) and the box then says "Hunt on" from that state, not from the pending edits — the toggle is the learner's choice, so it also shows for a section whose hunt was already enabled with no edit pending, and it is never bundled into `edit_plan`. It only appears when `section.hunt.available` is true, with `section.hunt.reason` as its description.
- **Approve is visible and disabled** while edits are pending, beside the primary "Send changes". "Discard" clears the edits and re-renders the card, so every control shows the plan's own values again and focus lands on the heading (the card's existing "was inside" rule). The "Comment for the planner" textarea is revealed above the action row while edits are pending.
- Badge order: the status word ("Done"), "You know this", then either "Not needed" or the aim (`aim: ⟨Level⟩`, or `⟨Level⟩ only` once the learner picks one), then "Hunt on", then "Needs install". Badges wrap by the badge, never mid-label, and the box is sized to the widest line.

#### 7.7.2 Known sections

- The section box of a section with `assumedKnown` is greyed (`--raised` fill, `--ink-3` text) and
  carries the badge "You know this", whose tooltip says "you said you know this". The title stays
  readable and nothing is struck through. Its drawer holds the full line "You said you know this.
  One quick check when we get here."
- In the plan sheet: "Known · not checked yet" → "Known · checked ✓" after a pass. After a miss (`knownRefuted`), the section renders normally and its line reads "Checked: needs teaching after all".
- The section opener in the flow (§7.2.6) for a known section reads "SECTION 2 OF 5 · QUICK CHECK".

**Shipped.** As above. On the card, a refuted section simply renders normally (no badge, no
greying): the learner is in review there, and the fact that a check was missed belongs to the
sheet, whose list now says "Checked: needs teaching after all". The opener's "· Quick check" comes
from `assumedKnown` alone, so a refuted section's already-drawn opener keeps it — the card and sheet
are where the change shows.

#### 7.7.3 Planner question on the card (`plan_ask`)

- Rendered inside the plan card, above the drawing, with the same markup as a choice block (§7.2.5). It is a question, not a wall of text, so it stays on the card rather than moving into a box's drawer.
- Kicker "The planner asks"; the submit button reads "Answer".
- While it's pending, "Send changes" and Approve are hidden: the question is the one action.
- When answered, it collapses to "You answered: ⟨choice⟩" until the new plan version replaces the card.

**Shipped.** The answered block is the flow's own "You" note plus "Waiting for the planner." rather
than a literal "You answered: ⟨choice⟩" line — the same treatment an answered question block gets
elsewhere (§7.2.4), and the label already says who spoke. The options come from
`pendingPlanQuestion.options`; the "other" free-text field is a radio with a text input, exactly as
in a choice block. The answer goes through `answer_plan_question({ sessionId, answer })` and the
block locks in place (no re-render), so a stray second click cannot send twice. The whole action
row — Send changes, Discard, Suggest a change — is hidden while the question is unanswered.

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

**Shipped (`ui/cards.js::huntCard`, item kind `hunt` in `timeline.js`).** The card is placed by the
`hunt_send` tool op (its `callId` is the `huntId`) and its lifecycle comes from `session_state.hunts`:
`applyHuntStates` refreshes each card in place from the record with its own `huntId`, so a return or a
skip never re-renders the flow, and a card from an earlier section keeps its own finished state when
teaching has moved on. A card whose section is no longer the current one shows "The section has moved
on. This hunt is closed." in place of the return form: nothing listens for a return once the section
has closed, and the backend refuses one anyway ("already finished").
Details and the two gaps:
- "Where to look" renders each `lookAt` entry as "⟨kind⟩: ⟨venue⟩ — ⟨why⟩" with the kind spelled out
  (forum thread, datasheet, lecture, hobbyist video, paper, docs, someone to ask); "Bring back" lists
  `bringBack` verbatim. "No deadline…" shows while the hunt is out. While `hunt.state` is `out` the
  running head reads "Teach ⟨n⟩/⟨m⟩ · Hunt" (§7.2.10's phase label, from `labelFor` in `cards.js`).
- Files: the form is a native `<input type="file" multiple>`; the app has no upload, so `files` is
  the list of **names**, and the returned block says "(not readable yet; the tutor has your
  description)" under each, as specified.
- The returned block shows the notes, then the links, then that phrase per file, and "Debrief in
  progress." — or "Debriefed. Teaching continues." once `hunt.debriefed` is true.
- **Not shipped:** the sorted-claim list from `debrief_record`. `session_state` carries no claims, and
  the `debrief` op's payload is not part of the UI contract, so
  rendering them would mean a second width of backend change. The debrief's prose reply is where the
  learner sees the sort for now.

#### 7.7.5 Close: field guide and calibration

After the close text, two disclosures (closed by default; these are reference material, not reading):
- **"Where the field lives"**:
  - "Useful now", grouped as Ask / Discussion / Reference;
  - "Later", with what unlocks each venue ("after op-amps").

  Each venue is a name, a link, and its one-line note. With depth `pass`, only Ask.
- **"Your confidence"**: one line per reported confidence choice ("Fairly sure: right 9 of 15"). Shown only when the calibration threshold is met.

Frontier items parked in the session appear as one line, "Saved for later: ⟨topics⟩", with a text button "Start a session" per item.

**Shipped (`ui/fieldguide.js`, `closeExtras` in `cards.js`).** The block is appended to the flow once
`session_state.ended` is true, so it sits after the close text and never mid-conversation; a session
with no venues, no calibration and no frontier adds nothing at all.
- **The now/later split** is the one rule §6 states outright. The durable venues live on topics in
  the graph (§7.6), but the **close is still the session's own judgement**: it lists the venues this
  session's brief found, not the topic's accumulated list. The brief does not carry a "useful now"
  verdict — its `entryLevel` is a phrase ("op-amps"), not a level id the UI could compare with held
  nodes — so the split follows the rule: an `ask` venue is useful now
  at every level; another venue is "Later" only when it has an `entryLevel` (shown as "— after ⟨entryLevel⟩");
  an explicit `usefulNow` / `now` boolean in the brief, if the researcher ever sends one, wins. With
  depth `pass`, only `ask` venues are listed and there is no "Later" group.
- The numbers and the sentences under "Your confidence" are used **verbatim** from
  `calibration.buckets` / `calibration.notes`; the UI never computes or rewrites them.
- Links open through `open_external` (the flow's existing link rule, §7.2.3).
- Frontier: `frontier()` filtered to `sessionId === this session`; each item is a "Start a session"
  text button calling `start_from_frontier({ itemId })`.

#### 7.7.6 Map page additions

- **Detail line:** adds "Reached: ⟨level⟩ · Aim: ⟨level⟩" when known. Box states stay the three of §7.6; the level is words, not a fourth visual state.
- **Guide:** a closed "Where the field lives · ⟨n⟩ topics" disclosure under the counts, one row per
  topic that has venues, with a button that jumps to that topic's cluster (§7.6). It comes from
  `knowledge_map.topicVenues`, not from a session.
- **Card:** the idea's card ends with its topic's venues, grouped Ask / Discussion / Reference (§7.6).
- **Calibration:** one line under the counts ("Your confidence: …"), when the threshold is met.
- **Frontier:** a "Saved for later" list, with a "Start a session" text button per item.

**Shipped (`ui/map.js`, `mapLearning()` in `app.js`).** Differences from the sketch, all from the
Map having no per-plan grouping — its unit is the topic:
- The guide belongs to the graph's topics, and it is rendered independently of any session: a Map
  whose newest Learn session is empty or absent still lists the topics that have venues, and a Map
  over an empty graph shows no guide at all even when a brief still lists venues. The close
  (§7.7.5) is the only surface that shows the brief's venues.
- `mapLearning()` therefore passes only levels, calibration and frontier — no venues. The rest
  under the counts is one block filled from the **newest Learn session's** `session_state` plus
  `frontier()`: the confidence line and a closed "Saved for later" list with a "Start a session"
  button per item. Hidden entirely when both are empty, so a Map with no sessions looks as it did.
- The level line is its own row on the idea's card (`Reached: … · Aim: …`), under "kind · topic",
  and it is also appended to the hidden list's description so the keyboard path gets it. `Reached`
  comes from `nodeLevels` (plan node id), `Aim` from the target level of the section that holds the
  node; the join to graph ids is the plan node's `graphNode`.
- The frontier list is the block's second disclosure rather than a separate page-bottom section: the
  page is the canvas, and a footer list under a pan/zoom stage read as part of the desk.

#### 7.7.7 Settings

- **Learning** section (new, between Model roles and Providers): "Starting point for new sessions", with the radios Measure me / Quick check / I'm new to this. The description reads "How much I check what you already know before planning. You can change it per session."

**Shipped.** One help line per option ("A few questions in each area, so the plan starts where you
do."; "At most five questions, on what would change the plan most."; "No questions. The first check
in each section does the measuring."), plus the section description above. The group reads
`get_settings().startingPoint` (default Measure me) and writes `set_starting_point({ value })` on
change, with "Saving…" → "Saved." or the error beside it; a failed save puts the stored choice back.

#### 7.7.8 Reference libraries (architecture §4.27)

**Home "+".** On Home in Learn mode (no session yet) the composer's "+" opens a small menu
(`#add-menu`, `role="menu"`, arrow keys move, Escape closes and returns focus to "+"):
"Files…" (the old picker), "Folder as a reference library…" with the note "Copied in; only its text
syncs", and, under "Link a library", every library already imported (name, file count; an already
linked one is disabled with "· linked"). Choosing a folder runs `library_import` with its progress
in the composer's status line ("Extracting 12 of 166: notes/digest.md"), then the result line
("huawei-ict: 166 files (166 added).") and a chip of kind "Reference library". Picking a folder
imported before refreshes it instead of duplicating it. The chip goes with the first message and
links the library to the new session; removing it before Start unlinks it (the library stays). On a
phone the folder item says folders can't be picked there, and linking an existing library works.
In a session "+" opens the file picker directly, as before; Practice has no "+". A library is never
a workspace and never goes through "Attach folder…".

**Settings → Libraries** (between Sync and Plugins). One row per library: name and id, topic and
description, "N files: 20 question banks · 94 guides · …" with scans without text, "From <folder> on
this device." or "Imported on another device: its text is here; refresh it where it was imported.",
and what the last import skipped by reason. Actions: Edit (name, topic, description — agents read
the description), Refresh (only on the importing device; progress and result in the section status),
Remove (confirmation: its text leaves every device at the next sync and this device's copies are
deleted; the folder is not touched). Below the list: "Skip files over (MB)" (default 50) and
"Import folder…".

### 7.8 Focus mode (full screen, silenced notifications, break nudge)

Sessions run for hours. Focus mode keeps the rest of the computer out of view while a session is open, and asks for one short break per 50 active minutes. Each part has its own Settings checkbox (§7.5), all on by default. They apply from the next session open.

- **Full screen.** Showing the session view (Learn, including ended and read-only, and Practice) makes the window full screen; "← Sessions" puts back the state it had before the session. The Map is reached from Home only, so it never inherits full screen. In Tauri this is `getCurrentWindow().setFullscreen()` (capabilities `core:window:allow-set-fullscreen`, `core:window:allow-is-fullscreen`); outside Tauri, `requestFullscreen()`, with refusals ignored. F11 toggles it anywhere in the session view, and the "More" menu has "Full screen" / "Exit full screen" (label read from the window each time the menu opens), so the learner can always get out without a shortcut. Reason: other windows, the taskbar and other apps are peripheral cues that trigger self-interruption [Dabbish 2011]; in a field study, a week without email meant fewer window switches and longer focus per window [Mark, Voida & Cardello 2012]. Tools that block distractions are a recognised class of digital self-control tools [Lyngs 2019]; that full screen helps learning here is [inference].
- **Silenced notifications (Linux, best effort).** On session open the app asks the backend (`focus_dnd(on: true)`) to turn on do-not-disturb, and turns it off on "← Sessions". The backend tries, in order, `makoctl` (mode `do-not-disturb`), `swaync-client` (`-dn`/`-df`, state `-D`), `dunstctl` (`set-paused`, `is-paused`), and GNOME `gsettings` `org.gnome.desktop.notifications show-banners` (only when `XDG_CURRENT_DESKTOP` contains GNOME). A backend counts only when its binary is on `PATH` and a live state query succeeds. If do-not-disturb is already on, the app leaves it alone and never turns it off: it undoes only what it did. A marker file (`focus-dnd` in the app config dir, naming the backend) is written when the app turns it on and removed when restored; restore also runs on app exit and, if the marker survived a crash, at the next start. `focus_dnd_support()` returns the backend name for the Settings label ("(mako)"), or `null` on macOS, Windows, or with no backend, and the option is then hidden. Reason: a notification that is merely received already harms performance on an attention task [Stothart 2015].
- **Break nudge.** A clock counts active time: a session view is shown, the document is visible, and the window has focus. It pauses otherwise. A pause of 5 minutes or more resets it, since that pause was the break [inference]. Once 50 active minutes have passed since the last nudge, the next turn boundary gets one static line in the flow, `--fs-small` `--ink-3`: "50 minutes in. Stand up for a minute." The boundaries are a section opener (the line goes just before it) and a finished tutor reply in Learn or Practice (the line goes after it). No timer is shown, nothing moves, and there is no extra announcement. The line is UI-only: no op is written, and it is gone when the session is reopened. A debug key, `localStorage["lh.debug.breakMinutes"]`, shortens the interval for testing. Reason: in a meta-analysis, micro-breaks raised vigor (d ≈ 0.36) and reduced fatigue (d ≈ 0.35), with no significant overall effect on performance (d ≈ 0.16), and longer breaks helped performance more [Albulescu 2022]. The nudge therefore aims at well-being over hours, not speed. Turn boundaries are where an interruption costs least [Adamczyk & Bailey 2004]. The 50-minute interval is the owner's choice, not derived from evidence [inference].

### 7.9 Momentum (resume on Home, practise at the end)

Two places used to end in a dead end: Home after an interruption (find the session in the list, then find your place) and a finished Learn session (a composer with nothing left to ask). Each now offers one next step. Neither adds a second primary.

- **Continue (Home).** When the newest session (`list_sessions`, newest first) is not read-only and can go on, the top of Home's start area shows one large *secondary* button: small-caps "Continue", then the title (600, one line, ellipsis) and, right-aligned in `--fs-small` `--ink-2`, where it stands. For Learn the place is the head's own current-phase label from `session_state` ("Teach 2/5", "Plan · review", "Measure"; `placeText()` in `cards.js`); for Practice it is "Practice". A Learn session whose state says `ended`, a read-only session, or a failed `session_state` call shows no block. `SessionSummary` carries no phase, so Home asks `session_state` for that one session only. Accessible name: "Continue ⟨title⟩, ⟨place⟩". Activating it is the list row's open path (`openSession`), so focus, alignment and the resumption cue are the same (§9.1). Start stays the one primary: Continue is a shortcut to something already on the screen (the newest row), not a new decision. The block re-checks on Home show, list load and delete.
  - Reason: interrupted tasks tend to be resumed; a meta-analysis puts the weighted resumption rate around 67 % (Ovsiankina effect) [Ghibellini & Meier 2025]. Cues that stay available cut the resumption lag (§2.2) [Altmann & Trafton 2004]. Putting the session one press away from the screen the learner lands on removes the search step that sits between the intention and the work [inference].
- **Practise these ideas (ended Learn session).** When `session_state.ended` is true (not for legacy read-only sessions, which keep their note), the desk shows "Session complete." and one primary button "Practise these ideas". It starts a Practice session with the plan's nodes, using the Map's message builder (`practiseText()` in `app.js`), so the first message is the same shape as the Map's "Practise": `Practise: ⟨titles⟩`, a blank line, "Mix them together rather than one at a time." and one line per node, "- ⟨title⟩ (in “⟨topic⟩”)" where the topic is the plan section that node belongs to (a node outside any section falls back to the plan's goal). The session view switches in place (no trip through Home), so Focus mode stays on. A plan without nodes gets "Start a new session" instead. No model: the note reads "Choose a model first." and Settings opens.
  - Reason: practice testing right after learning beats restudy [Adesope 2017] (§2.6), and mixing the ideas is what the Map already asks for. A next step at the end keeps the learner in the work instead of in a menu [inference].

### 7.10 Copy

Rules for every visible string (UI owner direction, 2026-11-06). Concise, scannable text measured 58 % more usable than promotional or wordy text in Morkes & Nielsen's web-writing study [Morkes & Nielsen 1997]; extraneous words are extraneous load (§2.3).

1. Delete any sentence that explains what a visible control already says. Example: the desk note for a pending exercise was "Answer the question to continue." next to "Go to question"; it is now "Question waiting.".
2. Labels at most three words; status and notes at most about six.
3. Errors keep cause and fix, nothing else ("Not in any registry here. Add one in Settings → Registries.").
4. Security and consent facts stay, once. The consent dialog's "Access" row says "No files, network or programs."; the extra note under it ("Plugins run isolated…") is gone.
5. Sentence case; no exclamation marks.
6. Screen-reader-only strings stay meaningful but short ("Enter sends. Shift+Enter: new line. @ mentions a file.").

The composer's vision note is gone: each image chip already says "This model can't see images" and Send asks before dropping them (§7.2.13).

---

## 8. Motion policy

**May move (exhaustive list):**

1. **Sheet and dialog open** (plan sheet, Settings): opacity 0→1 plus `translate` 8 px→0 (from the right on desktop sheets, from below on phone sheets) over `--dur-sheet-in` with `--ease-out`. Close: opacity 1→0 over `--dur-sheet-out` with `--ease-in`. The backdrop fades with the same timing.
2. **Hover and pressed color changes** on controls: `background-color`, `color`, `border-color` over `--dur-hover`, linear. These are foveal and user-caused.
3. **The Map page's canvas** (§7.6), and nothing else on that page: the opening burst, sparks,
   shockwaves, the pan/zoom/fit camera and the HUD count-up. Reason: the Map is a motivational
   surface the learner opens on purpose, between sessions, and every remaining effect answers the
   learner's own hand [Kao 2024]. The flowing light on edges, the ignite sequence, the needs-work
   halo, the selection orbit's rotation and the star twinkle are removed (owner decision,
   2026-11-07): they were untriggered motion, and with no per-claim check state they would have
   animated facts the graph does not hold. The rules below still bind every session view, and the
   canvas stops drawing frames entirely once the picture is still. Under reduced motion or forced
   colours the canvas is still (§7.6).
4. **Inside a plugin view, after the learner acts** (§7.2.15): a view may redraw or animate in response to the learner's own input (a slider, a button), and must stop when that response is done. It never animates on load or on its own, and under `prefers-reduced-motion` it changes state without motion. Reason: the motion is caused by the learner's hand and is the content being studied, like the Map's canvas; it is foveal, not peripheral (§2.1). The view is a separate document, so the main document's `getAnimations()` check below is unaffected; the view's own motion is checked by its plugin's author and in §11a's motion row.

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
- At runtime, `document.getAnimations()` is empty except during a sheet transition. (The Map's canvas uses `requestAnimationFrame`, not CSS or Web Animations, and only runs while the Map is shown.)

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
| A box's "⋯" opens its drawer | The drawer's heading (`tabindex="-1"`), or the box's "⋯" again if it was already open | None |
| The drawer closes (Escape, "Close", another box) | The "⋯" of the box whose drawer it was | None |
| Plan approved, composer returns | The desk textarea, only if not engaged elsewhere | None |
| "Go to question" / "Go to plan" in the desk | The first field or heading of the target | `scrollIntoView({block:"center", behavior:"instant"})` |
| "↓ Continue reading" | The desk textarea (or the desk note's button), because the activated button hides itself | Instant, as in §7.2.2 |
| Plan overlay opens / closes | The sheet heading / the "Plan" button | n/a |
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
| Esc | Plan overlay, Settings, a box's drawer | Close (native `dialog` cancel for the first two; the drawer's own handler restores focus to the box) and restore focus |
| Tab / Shift+Tab | Everywhere | Order: head (back → Plan) → page content in reading order (links, research disclosures, exercise fields) → desk |
| Arrow keys | Radio groups (mode, options, confidence, theme) | Native radio behavior |
| Enter / Space | A section box's "⋯" in the plan drawing | Open that box's drawer (or close it) — the "⋯" is a focusable `role="button"` in the drawing, reached by Tab in layer order |
| F11 | Session view | Toggle full screen (§7.8) |

Not added: Esc to stop a reply (too easy to trigger by accident; Stop is one Tab away in the desk).

### 9.3 Background events while the learner reads

| Event | Visible effect | Announced? |
|---|---|---|
| Research job queued, running, done or aborted | Job line text changes in place | No |
| Research failed | Job line shows the error | No (it is not blocking) |
| Planner running | Head label "Planning…", job line, desk note (when in the plan phase) | Head label has `role="status"` → once |
| Plan ready for approval | Plan card at the end of the flow, desk note "Plan ready." | "Plan ready for review." |
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
- **Attachments** (§7.2.13): a "+" button, paste, drag-and-drop and the `@` list add chips above the field (in Practice only images: no "+" and no `@` list, §7.3); chips are part of the draft, removed by a button whose accessible name names the item, and are not sent until Send. Image bytes stay device-local until Send; removing a chip deletes them. Enter inside the `@` list accepts the highlighted item (not while an IME is composing); Escape closes the list and keeps the typed text. Enter elsewhere still sends (Ctrl/Cmd+Enter on phone keyboards). On Home "+" is a menu that also imports or links a reference library (§7.7.8).
- **Vision warning:** when the Tutor or Practice role's catalog entry says it cannot see images, an image chip's kind line reads "This model can't see images", and Send opens one confirmation dialog before sending without them. It never silently drops a chip.
- **Workspace chip** (§7.2.14) lives in the running head, not the composer; it belongs to the session and is not affected by Send.

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
  - "Workspace attached: ⟨name⟩." / "Workspace detached." (§7.2.14)
  - "⟨Title⟩ installed." after an install from a plan-card chip (§7.2.16)
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
| 17 | "Reasoning" disclosure above every reply | Hidden in Learn; closed "Tutor's working" before the reply in Practice (§7.2.2, §7.3) |
| 18 | Two header bands (topbar plus phase strip) | One running head (§6.2) |
| 19 | Bold centered sans hero | Serif display line, left-aligned in the column (§7.1) |
| 20 | Plan side panel open by default | Plan overlay on demand (§7.2.9) |
| 21 | Confidence slider defaulting to 50 % | Five labelled radios, no default (§7.2.4) |

---

## 11. Constraints the implementer must keep

1. **Backend contract, exactly.** Use only the commands and events listed below, with the payload shapes given; Settings keeps its provider and role commands (`get_settings`, `list_providers`, `list_models`, `save_api_key`, `remove_credential`, `set_base_url`, `set_role_model`, `test_role`, `list_roles`, `role_usage`, the `workbuddy_login_*` and `claude_login_*` sign-ins, `open_external`). The Rust additions for the UI are exactly these:
   - the read-only `knowledge_map` for the Map page (§7.6);
  - nothing new for the plan overlay: it draws from `session_state` (`plan`, `completedSections`, `currentSection`, `checks`, `lastNodeId`);
   - the proposed-links review (§7.6): `edge_proposals() -> { proposals, titles }` and `decide_edge_proposals({ accept: string[], dismiss: string[] }) -> { accepted, rejected, removed, remaining }`. A proposal is addressed by `from\u0000to`; the core re-validates every acceptance (unknown id, self-edge, duplicate, cycle) and is the only writer of `graph/edges.jsonl`;
   - focus mode: `focus_dnd_support() -> string | null` and `focus_dnd({ on })` (§7.8);
   - the composer (§7.2.13): `pick_files() -> path[]`, `stage_image({ data, name? }) -> Attachment`, `unstage_image({ path })`, `stage_file({ sessionId?, path }) -> Attachment`, `attachment_data({ path }) -> data URL`, `mentions({ sessionId?, query }) -> Mention[]`, `vision_capabilities() -> { tutor, practice }`. An `Attachment` is `{ kind: image | file | folder, path, mediaType?, bytes?, name? }`;
   - the workspace (§7.2.14): `pick_folder() -> path`, `attach_workspace({ sessionId, path }) -> name`, `detach_workspace({ sessionId })`, `workspace_path({ sessionId }) -> path | null`;
   - plugin views (§7.2.15): `plugin_view({ pluginId })`, `plugin_view_url({ pluginId })`, `plugin_view_call({ pluginId, function, args })`, `view_state({ sessionId, artifactId, summary })`, and the shell's `pluginview` URI scheme that serves the view document (§7.2.17);
   - plugins and registries (§7.2.16, §7.2.18, §7.5.1): `plugins()`, `set_plugin_enabled({ id, enabled })`, `install_plugin({ registry, id, version })`, `add_folder_plugin({ path })`, `remove_plugin({ id })`, `missing_locked()`, `registries()`, `add_registry({ name, url })`, `remove_registry({ url })`, `refresh_registries()`, `registry_catalog()`, `install_candidate({ id })`;
   - sync (§7.1, §7.5.2): `sync_status()`, `set_sync({ remote, token, branch })`, `set_webdav({ url, username, password })`, `sync_now()`, `acknowledge_conflicts()`, `sync_if_changed()`, shapes in `docs/sync-build.md`.

   Nothing else in Rust changes for the UI.
   - **Events:** subscribe once via `subscribe_events({ onEvent: channel })`. The event kinds are `session`, `turnStart`, `text`, `thinking`, `op` and `turnEnd(error?)`.
     - Ignore events for other sessions, except `session` (list upsert).
     - Live text is replaced by the assistant `message` op, which is the persisted truth.
   - **Learn commands:**
     - `start_teaching({ text, attachments? })`
     - `send_learner_message({ sessionId, text, attachments? })`
     - `answer({ sessionId, callId, payload })`
     - `approve({ sessionId, removedPlugins:[{sectionId, plugin}] })` — the card now sends `removedPlugins: []`: tools are edited through `edit_plan` (§7.7.1)
     - `request_plan_change({ sessionId, text })`
     - **Learning system (§7.7):**
       - `edit_plan({ sessionId, edits, comment })` — one call per "Send changes". `edits` is
         `{ depth?, explore?, minutesPerSitting?, style?, sections?: [{ sectionId, targetLevel?, keep?, knownNote?, learnerNote?, plugins? }] }`, absent fields omitted, `plugins` the list the learner kept; `comment` is a string (empty when they wrote none). The button is disabled while a planner job runs, because the core refuses a second one.
       - `answer_plan_question({ sessionId, answer })`
       - `set_hunt({ sessionId, sectionId, enabled })` — the learner's toggle; no planner job
       - `hunt_return({ sessionId, huntId, payload })` — `payload` is `{ notes?, links?: string[], files?: string[] }`, all optional, and the UI sends `{}` when the learner returns with nothing (it asks once first)
       - `hunt_skip({ sessionId, huntId })`
       - `set_starting_point({ value })` with `value: measure | quick | new`; `get_settings` returns `startingPoint`
       - `frontier() -> [{ id, topic, why, at, sessionId }]` and `start_from_frontier({ itemId })`
     - `session_state({ sessionId })` — debounced ~50 ms after `phase`/`plan`/`approve`/`pluginGrant`/`tool` and any learning-system op (`planEdit`, `planAsk`, `planAnswer`, `knownRefuted`, `huntToggle`, `huntSend`, `huntReturn`, `huntSkip`, `debrief`)
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
   - **Practice** keeps `send_message({ sessionId, mode:"practice", text, onEvent })` and `retry_reply` with per-call channels, and adds `attachments?` (images only, §7.3). Learn never calls `send_message`.
   - **Legacy** `readOnly` sessions: render read-only, no composer, "Start a new session" → Home.
   - **Mobile lock** (`readOnlyReason === "desktop-plugin"`): render read-only with the desktop-plugin note; "Continue here anyway" → `mobile_unlock({ sessionId })` → re-open (§6.3, §7.2.12).
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
4. **No new dependencies beyond the vendored set, no bundler, no runtime font downloads.** System font stacks only (§5.1). Vendored marked, DOMPurify and KaTeX are unchanged; KaTeX's CSS is only overridden for `font-size` (§5.2). The outputs build added two vendored libraries, both loaded on demand (`docs/architecture.md` §4.24): Prism (`ui/vendor/prism/`, core plus per-language grammars loaded when a finished code block needs one) and Mermaid (`ui/vendor/mermaid/`, loaded on the first `mermaid` block). Nothing is fetched from the network.
5. **Practice behavior unchanged except attachments** (§7.3): same commands, channels, retry and recovery, auto-title. Its composer takes images only (pasted or dropped; no "+", no `@` list), sent as `attachments` on `send_message`. Otherwise only its rendering changes.
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
and dark; `learning_map` itself was checked against real op-log files in a throwaway test. (The
command is `knowledge_map` since the 2026-11-07 graph rebuild; see the revision below.)

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

### Outputs, plugins, composer and workspace build (verification, 2026-11-06)

Verified in headless Chromium 155 against a `window.__TAURI__` mock (`/tmp/lh-bench/mock2.js`
plus `/tmp/lh-bench/mock3.js`), with `ui/` served over HTTP under the app CSP from
`crates/app/tauri.conf.json` and views served from a separate origin under the `VIEW_CSP`
header from `crates/app/src/lib.rs`. Screenshots at 1440×900 and 390×844, light and dark;
`forced-colors: active`, `prefers-reduced-motion: reduce` and light/dark were emulated through
CDP. No console errors on the exercised paths.

| Check | Result |
|---|---|
| F2 20 000-char stream, 12 code blocks, 8 runs across the pass | **0 frames > 16 ms** in every run; max frame after 1 s **6.3–14.8 ms** (earlier baseline 9.2–10.2 ms, §11a); p95 after 1 s **4.2–6.4 ms**; 740 deltas per run; 1.7–2.8 s total JS work; finalize 1.0–2.1 ms. Output identical to the single-pass renderer: 288 KaTeX, 12 `<pre>`; all 12 blocks highlighted at finalize; 0 long tasks during streaming (the one 61–93 ms long task at ≈12.4 s in earlier runs is the bench's own final-text sanity render after streaming). **F2's pass criterion is the count of frames over 16 ms, which stayed 0;** max frame and p95 are context, not criteria. The 14.8 ms high is a single frame in one run. |
| F2 startup loads | `marked`, DOMPurify, KaTeX CSS/JS and Prism core only: no Mermaid, no Prism grammar. Mermaid 2 749 172 B loaded on the first `mermaid` block; the reply's Python grammar loaded only when its block was final. |
| W1 Mermaid | A valid `flowchart` renders an SVG with all text labels (12 V, R1, R2, ground); an invalid block keeps its `<pre>` source and gains "Diagram error: Parse error on line 3…"; Mermaid's scratch node is removed. |
| W1 highlighting | Light token/background contrast: plain 15.10:1, keyword 6.79:1, comment/punctuation 8.78:1, number 15.10:1; dark: 12.66:1, 8.05:1, 8.43:1, 12.66:1. Under `forced-colors: active` every token computes to the system text colour and weight 400. |
| W3 keyboard-only from Home | Shift+Tab → "+" → Enter stages an outside-home file → Shift+Tab → Remove returns focus to the textarea → `@ohm` opens the listbox with `aria-activedescendant` → Enter inserts `@/home/me/notes/ohms-law.md ` and a chip → Escape closes the list → pasted image adds a third chip → Enter sends all three on the first learner message op. Reopening the message shows the image thumbnail. `scrollWidth = 390` at 390 px. |
| W3 vision `false` | Image chip's kind line "This model can't see images", note above chips; Send opens the confirmation with focus on "Keep editing"; Escape keeps text and chips; confirming sends only the file. |
| W4 workspace | "More" menu keyboard: Enter focuses "Attach folder…", attach names the folder in the head chip and writes a `workspace` op with the name only; Detach removes it; Escape closes and returns focus. A folder chip's "Use as workspace" attaches the mentioned folder. |
| W6 artifacts | A referenced artifact appears mid-paragraph as a figure; an unknown id is "[Missing figure]"; an unreferenced one is appended after the message in creation order. Removing the plugin leaves both as inline SVG (`role="img"`, alt as its label). A live-streamed turn resolves `artifact:` at finalize. An SVG with `<script>`, `onload`, `onclick`, `foreignObject`, `javascript:` and external `url(...)` loses all of them in the UI while keeping `<rect>` and all 11 `<line>`s. |
| W7 views | The circuit view's R2 slider (keyboard ArrowRight ×5) changes Vout from 3 V to 4 V, redraws the schematic, and posts `state` "R2 = 1.5 kΩ, Vout = 4 V"; opening a session posts no state. A disallowed `circuit_draw` call gets "`circuit_draw` is not available to this view"; an allowed `solve` returns in 5.5 ms (host-side cold VM: 2.9–7.1 ms, 20 calls, all < 200 ms). Focus Tab from the slider reaches Submit; Enter submits `{ probes, values }`, the frame goes inert, and the graded result reaches the tutor without a verdict. A name-not-in-calls call from inside the view is refused. |
| W8/W9 plugins UI | Plan chip "Circuits — install" → consent (title, description, version, source, "No access to your files, network or programs.") → install turns the row into a checked checkbox; a corrupt hash shows the error in the dialog and leaves the chip; a lock entry missing on this device offers Install; Add registry by keyboard + Refresh reports "Refreshed 2 registries." Keyboard-only reinstall from the lock works. |
| Motion | With `prefers-reduced-motion: reduce`: 0 running animations idle, opening the plan sheet, opening the More menu and moving a view slider. |
| Security | `srcdoc` inline script blocked under the app CSP; the same script runs without it and from the view origin with `VIEW_CSP`. IPC-escape probe: every attempt failed (details in `docs/plugins.md` §7.2); `sendBeacon` returned `true` but no request reached the server. |
| Reproducible checks (2026-11-07) | The /tmp mocks above are superseded for W6/W7 by committed checks. `scripts/ui-check/run.sh` serves `ui/` and the views the same way (app CSP, `VIEW_CSP`, Tauri's Windows/Android origins via `--host-resolver-rules`) with a committed fake bridge, and runs two cases in headless Chromium 156: `ipc` (the probe view `plugins/fixtures/ipc-probe`: 49 attempts, 0 escaped, 0 requests leaked, no stray bridge calls) and `placement` (the op order `plugins_e2e.rs` asserts for the core: A1 lands mid-sentence inside the reply's paragraph, A2 and A3 follow the message in creation order, A3's missing plugin shows the fallback; Tab reaches the R2 slider, ArrowRight ×5 moves R2 1000 → 1500 Ω, the bridge solves each step, the probe row reads 4 V and the last `state` line is "R2 = 1.5 kΩ, Vout = 4 V"). The fake bridge's `solve` covers only the divider fixture; the real solver is tested in `plugins/circuit/tests` and `plugins_e2e.rs`. |

The `pluginview` scheme itself only exists inside Tauri, so the browser checks use a separate
HTTP origin with the same CSP header and a mocked `window.__TAURI__`. WebKitGTK and the mobile
webviews are still unverified; the manual probe run is in `docs/plugins.md` §7.2
(`docs/agent-system-plan.md`, Known gaps).

Not re-measured here (unchanged from §11a): A3–A6, F1, M1–M3, E1–E7, P1–P4, B1–B4, S1–S5 and
K1/K2 of the earlier build.

### Knowledge map rebuild, plan diagram and the ported session (verification, 2026-11-07)

Served `ui/` over HTTP with a `window.__TAURI__` mock built from the real data repo
(`~/.local/share/com.pslearningharness.app/repo/`): the session written by
`port_learnharness_session`, its replayed `SessionState`, and the real `knowledge_map` payload
(19 topics, 49 claims, 35 edges, 4 topic totals). Headless Chromium 156 via the DevTools protocol,
1280×950; five screenshots. This is the same shape as `scripts/ui-check/run.sh` but ad hoc (the
committed checks cover plugin views only); the throwaway driver was deleted.

| Check | Result |
|---|---|
| Ported session, content | Opening `Calculus \|-|||` mounts 40+ items: 26 learner messages, 97 tutor messages with the source's explanations, 150 "Predict first"/"Check your understanding"/"Quick question" cards with the learner's answer, reasoning, confidence and the source's own correct/not-quite verdict, 7 section openers and 2 compaction dividers. The head reads **Teach 5/9** (4 topics finished, the next unfinished one is 5) and the Plan button is available. |
| Ported session, math | Question prompts **and options** render KaTeX (options go through `renderMarkdown` + `enhanceLive`; before, an option rendered as `$\\Delta y\\Delta x$`). Learner answers with formulas render the same way. |
| Plan card (await approval) | Topic diagram: 9 boxes, 4 marked done, 2 cross-topic arrows, caption present, 1:1 at the column width. The section list below is unchanged. *(Superseded 2026-11-07 by the flowchart-first card below: the list is gone.)* |
| Plan overlay | Full window (1280×950 = the viewport), `::backdrop` `blur(14px) saturate(0.85)`, no scroll of the overlay itself. Opens on **Ideas** (49 boxes, 35 arrows); the current topic's claim carries the gold mark (`stroke rgb(192,138,42)`, drop-shadow); no claim claims to be individually current (none was ever tagged). Wheel zooms (1.29 → 2.03), a drag pans, `[−] [+] [Fit]` present, hint present. Switching to **Topics** draws 9 boxes, 4 done, the current one gold. The drawer lists 9 sections with 1 current and holds the change form. Real Esc key closes it and focus returns to `plan-toggle`. |
| Plan diagram geometry | No two boxes overlap and no text spills outside its box, in either level (audited with `getBBox()` over all 49 / 9 boxes; before the width slack one long word ran 4 px under the next box). |
| Map physics | Stiffened: settle 236 ticks at the first constants and **175** at the shipped ones (was 343 before), mean distance from the layered home slot 144 (was 174), and a flung claim overshoots 14.7 units instead of 28.5 (fling speed capped at 20, not 40). Measured by running `ui/map-physics.js` headless on the real graph. |
| Graph acyclicity | The source recorded one 2-cycle (`Punctured-neighborhood dependence` ↔ `Continuity at a point`); the importer now drops the later direction (`dropped 1 prerequisite edge(s) that would close a loop`) and `edges.jsonl` is a DAG again — 35 edges. `extract_edges --dry-run` no longer reports a pre-existing cycle. |
| Map: structure | 9 clusters (`Mathematics › … › ⟨leaf⟩`), 49 claims, 35 recorded prerequisites, HUD `49 ideas · 9 topics · 35 recorded prerequisites`; 4 topic subtitles carry the real totals (`30 answers · 1 wrong`, `34 answers · 11 wrong`, `6 answers · 0 wrong`, `3 answers · 0 wrong`). 29 claims have no recorded prerequisite and the card says so (`No prerequisite recorded`). |
| Map: static edges | The canvas settles at ~sample 19 of 40 (200 ms apart) and then two frames 350 ms apart are byte-identical `toDataURL()` — no travelling light and no idle loop. This is the check the old particles failed. |
| Map: card and flyout | Focusing a claim shows the description card (title, `mechanism · … path`, the real `statement`, and the labels — `No prerequisite recorded` on a root claim, `Leads to (1)` on the one that leads somewhere); focusing `Depends on` opens the side box with `Domain of a composition` on its own row. |
| Map: titles | Cluster titles are screen-space and placed above/below with collision rejection; at whole-graph zoom the two titles that cannot be placed are dropped rather than overprinted. |
| Map: a11y | 9 fieldsets (legends = full topic paths), 49 checkboxes labelled "⟨title⟩, ⟨kind⟩" with `aria-describedby` their depends-on list, two focusable label buttons per claim. |

### Flowchart-first plan card and the widened measure (verification, 2026-11-07, owner-requested)

Same harness shape as `scripts/ui-check/run.sh` (serve `ui/` as `http://tauri.localhost` under the
app CSP, inject a `window.__TAURI__` mock before the modules, drive Chromium over the DevTools
protocol). Throwaway driver, deleted after the run; the card was checked at 1280 and 1440 px and the
measure at 1280 and 2560 px.

| Check | Result |
|---|---|
| Card shape | A plan in `awaitApproval` renders `figure.plan-figure > .plan-diagram` with 3 boxes and 3 `.plan-node-action`s; `document.querySelectorAll('.plan-sections')` is **0** (the editable list is gone); the approach line and the Approve primary remain; the drawing's root is `role="group"`. |
| Corner expand | `.plan-expand` (accessible name "Expand the plan to the full screen") opens `dialog.plan-overlay[open]` with the same 3 boxes, `Topics` pressed, and the bar reading "3 sections · not approved yet". |
| Box state | Badges read `aim: Use`; the `assumedKnown` section is `is-known` with "You know this"; a `knownRefuted` section is back to normal; the missing plugin adds "Needs install". No box is `is-current` before approval. |
| Drawer | Clicking a box's "⋯" opens `.plan-node-drawer` (`position: fixed`, `inset-inline-end: 0`) with that section's title, `Mechanism · socratic · Aim: Use`, "Goal: …", "Covers: One current (mechanism)", the planner's notes, Tools/How far/Include, both textareas, the Hunt toggle with its reason and the install chip. `aria-expanded` is true on that box only; `aria-controls` names the drawer; focus enters the drawer's heading. Enter on another box's "⋯" swaps the drawer to it. |
| Pending edits | "How far → Design" in the drawer leaves the drawer open, turns the box's badge to "Design only", disables Approve and shows "Send changes". One click sends **one** `edit_plan` `{ sessionId: 's1', edits: { sections: [{ sectionId: 's2', targetLevel: 'design' }] }, comment: '' }` — no `approve` call. |
| Hunt | The drawer toggle fires exactly one `set_hunt { sessionId, sectionId, enabled: true }`, sends no `edit_plan`, the box reads "Hunt on", and the drawer survives the state refresh. |
| Close paths | Escape closes the drawer and returns focus to its box's "⋯" (every "⋯" reads `aria-expanded="false"`); the "Close" control does the same; Discard re-enables Approve and restores the plan's own badges. |
| Measure | The same tutor paragraph measures **1028.5 px** at a 1280 px viewport (`--measure: 92ch`) and **1162.7 px** at 2560 px (`104ch`), against **626.1 px** for the old fixed `56ch`; `pre` and `table` stay `overflow-x: auto`/`display: block`, and the page never scrolls sideways. Home (1077 / 1211 px column, no overflow), Settings (640 px sheet) and the Map (full window) lay out at 1280 and 2560 px. |
| Existing checks | `scripts/ui-check/run.sh ipc placement` still passes (49 attempts, 0 escaped; the A1/A2/A3 placement and R2 slider case). |

---

## 12. Acceptance checklist (verifiable with screenshots and small scripts)

Use the mocked backend: serve `ui/` with `index.html` patched to load a `window.__TAURI__` mock first. A mock like `/tmp/lh-bench/mock.js` provides sessions `L1` (await approval), `long` (500 messages, teach), `legacy` (read-only) and `pr` (practice), plus `__mock.emit(event)`. Screenshot each item at **1440×900** and **390×844**, in **light and dark**.

**Look**

- [ ] A1 The session view shows only the running head, page column and desk. There is no sidebar, model pill, avatar, gradient, pill-shaped element or drop shadow.
- [ ] A2 At 390 px the session title is visible in the head.
- [ ] A3 Tutor prose's computed `font-family` resolves to the serif stack. Its `font-size` is 20 px (desktop) / 18 px (phone) and its line-height is 31 px / 27 px.
- [ ] A4 Characters per line, measured on a long tutor paragraph (canvas `measureText` average-character method, or count characters on the first full line), are **58–72 below 1024 px and ~90–100 at ≥ 1280 px** (the stepped measure, §5.2; measured 1028.5 px of prose at 1280 and 1162.7 px at 2560).
- [ ] A4a The column is not full bleed: at 2560 px the paragraph is ≈ 45 % of the window, and code blocks and tables still scroll inside it rather than wrapping.
- [ ] A5 Contrast spot-check with computed colors: body ink ≥ 7:1, `--ink-3` text ≥ 4.5:1, control borders and focus ring ≥ 3:1, in both themes (values in §5.3).
- [ ] A6 A grep of `styles.css` finds no `linear-gradient`, no `999px`, no `text-transform: uppercase`, no `box-shadow`, and no `infinite`.

**Motion and scroll**

- [ ] M1 During streaming and while a research job runs, `document.getAnimations().length === 0`.
- [ ] M2 Start a stream with `__mock.emit` while scrolled up by 600 px, and emit 30 text deltas. `#messages.scrollTop` is unchanged (±1 px), checked before and after with two screenshots.
- [ ] M3 With the learner at the end, a new turn whose start would be below the fold is aligned once under the head; `scrollTop` then stays constant for the rest of the stream.
- [ ] M4 With `emulateMediaFeatures([{name:"prefers-reduced-motion", value:"reduce"}])`, opening the plan sheet produces no running animations or transitions.
- [ ] M5 No toast element exists in the DOM; `#toast` is gone.

**Exercises**

- [ ] E1 For a pending `question_ask`, the desk shows "Question waiting." and the composer is `hidden`. The block is open even for `role: "check"`.
- [ ] E2 For a pending question, `document.body.innerText` does not contain the mock `args.answer` (use a distinctive answer string).
- [ ] E3 No confidence radio is checked initially. Submitting with an answer but no confidence shows "How sure are you? Pick one." and does not call `answer`.
- [ ] E4 Choosing "Fairly sure" and submitting calls `answer` with `confidence: 70`. "I don't know — show me" calls `answer` with `dontKnow: true` and no `confidence`. Check via `__mock.calls`.
- [ ] E5 After a `verdict_record`, the block shows "✓ Correct." (no confidence echo, no `note`) and the explanation, in ink color, with no pill.
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
- [ ] S3 Home: the mode control is a radio group with descriptions. There is no "Or try:" line and no visible Enter hint. The list shows group headings and rows at least 48 px tall. Each row's "…" opens Extract (greyed below a quarter of the automatic threshold) and Delete; Delete needs two presses.
- [ ] S4 Settings: role fieldsets with inline status after "Use this model". Appearance → Dark switches the theme, and the choice persists across a reload.
- [ ] S5 At 390 px nothing scrolls horizontally (`document.documentElement.scrollWidth <= 390`), and every button is at least 44 px tall under `pointer: coarse` emulation.

**Keyboard-only walk**

- [ ] K1 Using only the keyboard: Home → type a topic → Enter → (mock) the question arrives → type an answer → arrow to a confidence → Ctrl+Enter → the verdict appears → Tab reaches the desk.
- [ ] K2 A visible focus ring at every Tab stop, never hidden under the head or desk.

**Performance**

- [ ] F1 Opening `long` mounts ≤ 40 timeline items. Scrolling to the top prepends more without a visible jump.
- [ ] F2 A 20 000-character streamed reply (bench page, if present) still renders with the block cache: completed blocks are not re-rendered, which you can check by counting `renderBlock` calls or comparing with the existing bench result. Pass: **0 frames over 16 ms** after the first second; max frame and p95 are recorded for context only.

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
- Mark, Voida & Cardello 2012 — "A pace not dictated by electrons": an empirical study of work without email. *CHI '12*. https://doi.org/10.1145/2207676.2207754
- Lyngs et al. 2019 — Self-control in cyberspace: applying dual systems theory to a review of digital self-control tools. *CHI '19*. https://doi.org/10.1145/3290605.3300361
- Albulescu et al. 2022 — "Give me a break!" A systematic review and meta-analysis on the efficacy of micro-breaks for increasing well-being and performance. *PLOS ONE* 17(8). https://doi.org/10.1371/journal.pone.0272460
- Weiser & Brown 1996 — The coming age of calm technology. https://calmtech.com/papers/coming-age-calm-technology
- Ghibellini & Meier 2025 — Interruption, recall and resumption: a meta-analysis of the Zeigarnik and Ovsiankina effects. *Humanities and Social Sciences Communications*. https://doi.org/10.1057/s41599-025-05000-w

Cognitive load, coherence, flow
- Sweller, van Merriënboer & Paas 2019 — Cognitive architecture and instructional design: 20 years later. *Educ. Psychol. Rev.* https://doi.org/10.1007/s10648-019-09465-5
- Ginns 2006 — Integrating information: spatial and temporal contiguity meta-analysis. *Learning and Instruction*. https://doi.org/10.1016/j.learninstruc.2006.10.001
- Höffler & Leutner 2007 — Instructional animation versus static pictures: a meta-analysis. *Learning and Instruction* 17(6). https://doi.org/10.1016/j.learninstruc.2007.09.013
- Rutten, van Joolingen & van der Veen 2012 — The learning effects of computer simulations in science education. *Computers & Education* 58(1). https://doi.org/10.1016/j.compedu.2011.07.017
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
- Morkes & Nielsen 1997 — Concise, SCANNABLE, and Objective: how to write for the web. Nielsen Norman Group. https://www.nngroup.com/articles/concise-scannable-and-objective-how-to-write-for-the-web/

Learning map (§7.6)
- Purchase 1997 — Which aesthetic has the greatest effect on human understanding? *GD '97*, LNCS 1353. https://doi.org/10.1007/3-540-63938-1_67
- Sugiyama, Tagawa & Toda 1981 — Methods for visual understanding of hierarchical system structures. *IEEE SMC* 11(2). https://doi.org/10.1109/TSMC.1981.4308636
- Gansner, Koutsofios, North & Vo 1993 — A technique for drawing directed graphs. *IEEE TSE* 19(3). https://doi.org/10.1109/32.221135
- Burch et al. 2011 — Evaluation of traditional, orthogonal, and radial tree diagrams by an eye tracking study. *IEEE TVCG* 17(12). https://doi.org/10.1109/TVCG.2011.193
- Misue, Eades, Lai & Sugiyama 1995 — Layout adjustment and the mental map. *JVLC* 6(2). https://doi.org/10.1006/jvlc.1995.1010
- Kao, Ballou, Gerling, Breitsohl & Deterding 2024 — How does juicy game feedback motivate? Testing curiosity, competence, and effectance. *CHI '24*. https://doi.org/10.1145/3613904.3642656 (abstract read: success-dependence raised all motives; amplification lowered them)
- Hicks, Gerling, Dickinson & Vanden Abeele 2019 — Juicy game design: understanding the impact of visual embellishments on player experience. *CHI PLAY '19*. https://doi.org/10.1145/3311350.3347171 (finding as summarised in Kao 2024; full text not read)
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
