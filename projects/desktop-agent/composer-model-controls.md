# Composer model controls

The model and reasoning selectors now sit inside the chat composer immediately before Send, following the supplied ChatGPT desktop reference. Native selectors retain keyboard and accessibility behavior. Selected-label sizing avoids excess space from long unselected model names; long selected names truncate. The muted Reasoning label, brighter value and double chevrons preserve the requested visual hierarchy. Project/access controls occupy the preceding row; Plan stays left and Stop follows Send.

Behavioral integration tests were RED before moving the controls, then GREEN. A browser width assertion was RED at 190px before selected-label sizing, then GREEN. All 115 frontend tests passed, all 5 browser scenarios passed, and the focused browser check passed again after restoring the circular Send button. Browser checks cover draft retention, model defaults, outgoing model/effort, and 800x600 layout including long names and a running turn. Sol found no functional or accessibility blockers.

The production frontend and signed desktop bundle built successfully. Signature verification passed. The updated app opened as a new instance while preserving the previous window; automatic approval review rejected terminating that window because it could discard unsaved state. Browser screenshots use desktop-protocol fixtures and do not invoke live inference.

Evidence: evidence/composer-model-controls-verification.json and composer-model-controls*.png. No dependencies, backend inference paths or runtime fork source were changed by this task.
