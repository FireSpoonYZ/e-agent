/**
 * Documentation-derived Pi 0.84.2 terminal-input fixture.
 * Source: docs/extensions.md TerminalInputHandler / ExtensionUIContext declarations.
 */
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
	pi.on("session_start", (_event, ctx) => {
		const unsubscribe = ctx.ui.onTerminalInput((data) => {
			if (data === "x") return { consume: true };
			return { data: data.replace("a", "b") };
		});
		pi.on("session_shutdown", () => unsubscribe());
	});
}
