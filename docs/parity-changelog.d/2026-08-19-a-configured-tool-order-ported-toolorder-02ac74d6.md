---
date: 2026-08-19
order: 30
---
A configured tool order ported (`ToolOrder`, `TOOL_ORDER_REST`, `TurnConfig::tool_order`; TC-PORT-LOOP-9..13), closing both upstream `tool-order` specs. The order is read against the registry it arranges, so a name nobody registered is refused before an engine exists rather than closing a no-step turn. A settings key for it is the remaining gap.
