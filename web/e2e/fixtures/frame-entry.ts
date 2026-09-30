import * as React from "react";
import * as ReactDOM from "react-dom/client";

const panelModule = await import("./malicious-panel");

export { React, ReactDOM };
export default panelModule.default;
