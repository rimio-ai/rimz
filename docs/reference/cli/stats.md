# Stats CLI

`rimz stats` prints your account-global token and dollar history: a heatmap of daily use, totals for a chosen window, and where the spend went by model and by agent. It reads only — it touches no agent, writes nothing to any session, and prints from the cache RimZ already keeps under its own state directory, so it runs inside a room or anywhere else on the machine, in or out of a project. Its one network call is the weekly price-table refresh, which `RIMZ_PRICING_OFFLINE=1` turns off. How every figure is calculated, and how the held dashboard reads, is the [Token Insight guide](../../guide/insight.md).

```sh
rimz stats                 # the all-time panel: heatmap, totals, model and agent breakdowns
rimz stats --dollars       # scale the heatmap by dollars spent instead of tokens
rimz stats --json          # the per-day buckets, windows, breakdowns, and insights for scripts
rimz stats --refresh       # hold the panel open and repaint every 60s (the rimzd daemon view)
```

| Flag | Effect |
| --- | --- |
| `--dollars` | Scale the heatmap by dollars spent rather than tokens used |
| `--json` | Emit the stats document instead of the panel; the stable surface for scripts |
| `--assists` | Print the complete newest-first system-assist timeline instead of the panel; conflicts with `--json` and `--refresh` |
| `--refresh` | Hold the panel open, refresh every 60s, and re-centre on resize; conflicts with `--json` |

A plain `rimz stats` prints the all-time window; in the held (`--refresh`) dashboard, `Tab` and `Shift-Tab` cycle the All time / Week / Month / Year window over the model, agent, and insight rows while the heatmap stays full-history.

The live slice of this same data sits in the sidebar's provider dashboard while you work; both read one source. The vocabulary the figures share, the window semantics, and the pricing model are in the [Token Insight guide](../../guide/insight.md).
