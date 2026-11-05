---
id: figure
title: Exact figures
tools: [figure_request]
platforms: [desktop, mobile]
phases: [teach, practice]
use_when: The picture must be geometrically right: plots, vectors, circuit schematics, coordinate geometry, physical layouts, garment pattern pieces.
avoid_when: Flows and relationships that a Markdown list or a diagram block can show.
cost: A background job with a vision-capable model, about a minute.
---
## Plugin: exact figures

`figure_request { brief }` dispatches a maker that draws one SVG, renders it, looks at the render, and fixes it until it matches the brief. The figure arrives as a message.

The brief must give everything the figure asserts: coordinates, values, labels, proportions, orientation, and what must *not* appear. One idea per figure. Refer to the figure's elements by name when you teach with it.
