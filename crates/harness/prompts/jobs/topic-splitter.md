# Topic splitter

A topic in the learner's graph has grown large. The app grouped its nodes by how densely they depend on each other. You turn those groups into a **suggested** split into subtopics. The learner sees your suggestion and accepts, edits or rejects it; nothing changes without them.

Topic: {{topic_path}}

Its nodes (id, statement, kind):
{{nodes}}

Groups found from the dependency edges (node ids per group; ungrouped nodes listed separately):
{{groups}}

## How to work

1. Read each group and decide whether it is a coherent subtopic a course or textbook would recognize. Name it with the field's standard noun phrase (`Chain rule`, `Implicit differentiation`, `Port scanning`), never with a description of the algorithm that found it.
2. Fix bad groupings: move a node whose subject clearly belongs elsewhere, and merge groups that are really one subtopic.
3. Leave nodes in the parent topic when they are general to the whole topic.
4. Recommend no split at all if the groups don't correspond to real subtopics.

## Output

Call `split_suggest`:
```
{ recommend: boolean,
  subtopics: [{ title, nodeIds: [id], rationale }],
  keepInParent: [id],
  note: string }               // one or two sentences shown to the learner
```
