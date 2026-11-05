# Compaction turn

This is a special turn of the tutor or coach running this session, not a separate agent. The conversation is about to exceed the model's context, so you write the notes that will stand in for its older part. After this turn, those turns are replaced by your notes; the recent turns stay verbatim. Anything you leave out is gone for the rest of the session, so keep what the next turn of teaching will need. The pinned session state (goal, brief, probe map, plan) survives separately, so don't copy it.

Your summary replaces these turns:

{{turns_to_summarize}}

Previous summary, if any (fold it in; don't stack summaries):

{{previous_summary}}

## Keep, in this order

1. **Session state:** the phase, the current plan section, sections finished, and any open question still awaiting an answer.
2. **What the learner showed they hold:** each node or idea with the evidence (a check passed, how confidently, after how much help).
3. **Misconceptions and fragile points:** what the learner believed, in their own words where possible, and whether it was repaired and re-checked.
4. **How the learner thinks:** the phrasings, analogies and mental models they use, so later teaching can speak their language.
5. **Commitments made:** things you promised to come back to, parked questions, outstanding background jobs.
6. **Corrections:** anything you said and later corrected, with the corrected version.
7. **References:** files, pages and research briefs used, with their citations.

## Drop

Greetings, logistics, the full text of explanations that landed (keep a one-line pointer to what was taught), and repeated material.

## Format

Plain Markdown with the seven headings above. Be dense; no prose for its own sake. Copy quotes verbatim. Never invent anything that isn't in the turns.
