# Interface design system

## Principle

One workspace, one visual language, one place for each kind of information. Density is a setting, not a default. A control appears where its effect is visible.

## Layout

```
+--------------------------------------------------------------+
| header 48   product · place · level · theme · units           |
+---------+---------------------------------------+------------+
| rail    | workspace                             | inspector  |
| 320     | map or radar                          | 340        |
|         |                                       |            |
+---------+---------------------------------------+------------+
| status 34   station · product · scan age · provenance         |
+--------------------------------------------------------------+
```

| Region | Default | Minimum | Maximum | Collapse |
|---|---|---|---|---|
| Header | 48 | 48 | 48 | Never |
| Left rail | 320 | 280 | 420 | Yes, to 44 icon strip |
| Workspace | remaining | 480 | none | Never |
| Inspector | 340 | 280 | 440 | Yes, to 0 |
| Status | 34 | 34 | 34 | Never |

The workspace takes all remaining space. Rails resize by a drag handle with keyboard support. Sizes persist.

## Spacing

Base unit 4. Only these values are used.

| Token | Value | Use |
|---|---|---|
| `space-1` | 4 | Inline gap, icon to label |
| `space-2` | 8 | Control inner padding, tight stacks |
| `space-3` | 12 | Group separation |
| `space-4` | 16 | Panel padding |
| `space-6` | 24 | Section separation |
| `space-8` | 32 | Empty-state blocks |

## Typography

| Token | Size | Weight | Use |
|---|---|---|---|
| `text-title` | 22 | 600 | Application title |
| `text-heading` | 18 | 600 | Panel headings |
| `text-subheading` | 15 | 600 | Group headings |
| `text-body` | 13 | 400 | Default text, values, labels |
| `text-strong` | 13 | 600 | Emphasised values |
| `text-caption` | 12 | 400 | Metadata, units, attribution |

Nothing renders below 12. Body text is 13 or larger. Line height is 1.35 for body and 1.2 for headings. Numeric readouts use tabular figures so values do not shift as they change.

## Colour

Tokens, not literals. Both themes define the same names.

| Token | Dark | Light | Use |
|---|---|---|---|
| `bg` | `#0B0E13` | `#F4F6F9` | Window background |
| `surface` | `#121720` | `#FFFFFF` | Panels |
| `surface-raised` | `#1A212C` | `#FFFFFF` | Cards, popovers |
| `border` | `#28313D` | `#D2D9E2` | Dividers, outlines |
| `text` | `#E6EDF5` | `#10151C` | Primary text |
| `text-muted` | `#9AA7B6` | `#4A5563` | Secondary text |
| `accent` | `#4C9AFF` | `#0B5FCE` | Selection, focus |
| `danger` | `#FF5A5A` | `#B3261E` | Failure |
| `warning` | `#FFB020` | `#8A5300` | Stale, degraded |
| `success` | `#3DD68C` | `#116B3C` | Live, healthy |

Semantic colour is reserved. `danger` means failure, `warning` means stale or degraded, `success` means current. A colour never carries meaning alone; text or icon accompanies it.

Contrast floor is 4.5:1 for body and 3:1 for large text and control outlines. The floor applies to every token pair in both themes. A unit test computes sRGB relative luminance for each pair and fails below the floor.

## Controls

| Token | Height | Padding | Radius |
|---|---|---|---|
| `control-sm` | 26 | 8x4 | 4 |
| `control-md` | 32 | 10x6 | 6 |
| `control-lg` | 38 | 12x8 | 6 |

Text inside a control is vertically centred by the control, not by a manual offset. Buttons, text fields, and selectors share one height per row.

Focus is a 2 point `accent` ring offset 1 point outside the control. Focus is always visible, including on map and canvas regions.

## Panel structure

Every collapsible section is a header row plus content.

```
[chevron] Section title                    [action]
content
```

The header is 32 high, uses `text-subheading`, and is fully clickable. The action slot holds at most one secondary control. Section state persists.

## Information hierarchy

| Rank | Content | Placement |
|---|---|---|
| 1 | Station, product, scan age | Status bar and workspace caption |
| 2 | Selected place and forecast | Inspector top |
| 3 | Active warnings | Inspector, above forecast |
| 4 | Layer list and controls | Left rail upper |
| 5 | Station and product selection | Left rail middle |
| 6 | History and playback | Left rail lower |
| 7 | Provenance, licence, diagnostics | Inspector bottom, expert level |

## Progressive disclosure

The level selector is in the header. It changes what is visible in the rail and inspector, not what is stored.

| Area | Basic | Standard | Expert |
|---|---|---|---|
| Products | Reflectivity, velocity | All source products | All plus derived |
| Layer panel | On/off | On/off, opacity | On/off, opacity, order, time policy, source |
| Legend | Shown | Shown, palette choice | Shown, palette, thresholds, range |
| Readout | Value and unit | Value, unit, range, azimuth | Value, unit, gate geometry, sweep, provenance |
| Diagnostics | Hidden | Cache state | Full source and timing |

## Themes

Theme is a persisted preference. This build implements `dark` and `light`; `system` is planned. Switching repaints panels, controls, text, the basemap, the map chrome, and the radar canvas fill. The radar canvas stays dark in both themes because the product colours are calibrated against it, and the legend keeps its dark backdrop. Two map chrome elements still draw their dark fills in light mode.

## Accessibility

| Requirement | Implementation |
|---|---|
| Text contrast | Token pairs tested against 4.5:1 |
| Focus visibility | 2 point accent ring on every focusable region |
| Keyboard reach | Every control reachable in reading order |
| Accessible names | Every control labelled, icons carry text alternatives |
| Scale | Interface scale 90 to 150 percent, persisted |
| Colour independence | Palette sets tested for deuteranopia, protanopia, tritanopia, and grayscale |
| Motion | Playback stops on request; no automatic animation without a visible control |

## Visual QA matrix

Each cell is inspected in a screenshot of the running binary. A cell is complete only when the render is correct at that combination.

| State | 1040x640 | 1280x800 | 1920x1080 | 2560x1440 |
|---|---|---|---|---|
| First launch, no site | Required | Required | Required | Required |
| Radar, single station | Required | Required | Required | Required |
| Radar, four layers | Required | Required | Required | Required |
| Map, world scale | Required | Required | Required | Required |
| Map, street scale | Required | Required | Required | Required |
| Map with warnings and measurement | Required | Required | Required | Required |
| Search results open | Required | Required | Required | Required |
| History playback | Required | Required | Required | Required |
| Empty, loading, stale, error | Required | Required | Required | Required |
| Expert panel expanded | Required | Required | Required | Required |

Every cell is inspected in dark and light themes. The matrix is 80 combinations. A release candidate requires all of them inspected, with defects fixed and recaptured.

## Current state

| Element | State |
|---|---|
| Tokens | Partial. `src/theme.rs` holds both palettes, the type scale, and spacing; residual literals remain |
| Type scale | Partial. Sizes route through `TYPE_*`; 10 to 17 point literals remain |
| Themes | Implemented: dark and light, persisted. Two map chrome elements still draw dark fills in light mode |
| Panel collapse | Absent |
| Focus ring | Absent |
| Accessible names | Not provided |
| Keyboard navigation | Search focus and Enter only |
| Scale setting | Absent |

The interface reported as cramped, undersized, and misaligned is a direct result of these absences. Correcting individual spacings without the token layer reproduces the defect.
