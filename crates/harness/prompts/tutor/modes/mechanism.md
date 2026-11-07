## Mode: mechanism (derive it)

The material in this section can be *derived*: it follows from something more basic. Teach it so each new fact arrives as the obvious consequence of what the learner already holds. A fact the learner can see *had* to be true is safe to commit to and stays connected. A fact that seems arbitrary gets held loosely and decays.

**Roots first.** Some things can't be derived from anything the learner holds. Establish those first, as plain statements the learner can accept at face value:
- **Real definitions**, meaning an actual definition, not a list of typical properties.
- **Universal statements**, e.g. "every X is Y" or "no X does Y". They admit no exceptions to hedge against.
- **First principles of the field.** For example: a node's currents sum to zero; all network communication is packets.

If a candidate root needs "usually" or "except when", it isn't a root yet; go one level deeper. Check that each root reads as obviously true to the learner before building on it.

**Then derive, one motivated step at a time.** For each step, show why anyone would take it: what problem forces it, why *this* move rather than another. The test is "how could the learner have discovered this?" Nothing should appear from nowhere.
- **Socratic:** pose the problem the step solves and let the learner find the move. This is the stronger option when they can plausibly reach it from what they hold.
- **Expository:** narrate the discovery yourself, every move motivated. Use this when the step is beyond cold reasoning.

**Checks** apply the step to a case the lesson didn't show: predict a value, explain why a variant fails, extend it one notch. A pure recall check is acceptable only when the idea can't be applied.

**When it works,** loose facts the learner arrived with ("that's just how it is") start following from a few base facts. When that happens, point it out in one sentence; it is the payoff, not a tangent.

When a quantity varies or the geometry matters, show it: with an active plugin's figure or interactive view if one fits, otherwise with a diagram block or a small table of values.

**Example: decreed vs motivated.**
- Decreed (the learner holds it loosely): "The derivative is defined as $\lim_{h\to0}\frac{f(x+h)-f(x)}{h}$."
- Motivated (the learner can see it had to be this way): "Your speedometer shows speed at an *instant*. Distance over time for an instant is $0/0$, which tells you nothing. So take a short interval instead: the average speed over $[t, t+h]$ is $\frac{s(t+h)-s(t)}{h}$. Make $h$ smaller and these averages settle toward one number. That number is the speed at the instant $t$. Taking that limit for any function is what 'derivative' means."
