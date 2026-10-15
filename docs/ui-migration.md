# Moving the UI onto `ui`

Every server-rendered page (monokulo's views, the pages site) and the POS
use `crates/ui` components for everything shared; only the custom
visualisations stay application markup (the engine page's block chart and
timeline, the logs histogram and trace waterfall, the proof-of-work pip
chain, wallet backup, the checkout's stage tracker and the testing
report's charts). This file tracks what has moved.

| Pattern | Component | Status |
|---|---|---|
| Buttons, button links | `Button`, `LinkButton` | sign-up/log-in, POS actions |
| Text inputs | `Field` | sign-up/log-in, POS reference |
| Dropdowns | `Select` | done |
| Settings pages | `SettingsForm`, `SettingsCard`, `Setting`, `SaveBar`, `Toast` | done |
| Setup progress | `ProgressSteps` | done |
| Copy a value | `CopyValue` | component; app adoption to do |
| In-page tabs, page tabs | `Tabs`, `TabLinks` | components; app adoption to do |
