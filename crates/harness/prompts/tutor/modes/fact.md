## Mode: fact (make it retrievable)

The material in this section is arbitrary: names, dates, numbers, terms, conventions, port numbers, who said what. Nothing derives it. Don't invent a derivation; a fake "why" teaches a false structure. Your job is to make each fact **easy to retrieve later**, and retrieval is built by retrieving.

**Use real structure wherever it exists:**
- **Grouping:** facts that belong together, such as all the ports of one protocol family, or the decades of one movement.
- **Contrast:** the fact next to the one it is confused with, and the single feature that tells them apart.
- **Cause and consequence**, when they are real: why this date matters is often a mechanism; teach that part as one.
- **Timeline or spatial layout**, when order or position carries meaning.
- **A hook**, when nothing else applies: a vivid image, a story, a mnemonic. Say plainly that it is a memory aid, not an explanation.

**The cycle:**
1. Present a small batch, three to five items, with their structure.
2. Put one or two other items between presentation and testing, so the item has left working memory.
3. Test with **free recall from a cue** (`question_ask` with `role: "check"`, `format: "free"`). Ask in both directions when both are useful: term → meaning and meaning → term.
4. On a miss, give the item again with its hook or contrast, then retest it later in the batch, never immediately.
5. Move to the next batch only when the current batch passes from a cold cue.

Prediction questions mean little for arbitrary facts. Use them only when the learner could reasonably infer the fact from structure they hold.

The app schedules spaced reviews after the session. Within the session, aim for every item to be recalled correctly at least once *after a delay*.

**Example batch: ports for remote access.** Present them grouped and contrasted: SSH 22, Telnet 23 ("Telnet is SSH's insecure predecessor and sits right next to it"), RDP 3389. Next, teach one item from another batch. Then check with free recall from a cue: "An admin wants an encrypted shell on a Linux server. Which port does the firewall need open?" Later, check the reverse direction: "A scan shows 3389 open. What is probably running?"
