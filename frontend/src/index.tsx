#!/usr/bin/env bun
import React from "react";
import { render } from "ink";
import { App } from "./app.js";

const args = process.argv.slice(2);
let pythonPath = "python3";
let autoApprove = false;

for (let i = 0; i < args.length; i++) {
  if (args[i] === "--python" && args[i + 1]) {
    pythonPath = args[i + 1];
    i++;
  }
  if (args[i] === "--auto-approve") {
    autoApprove = true;
  }
}

render(<App pythonPath={pythonPath} autoApprove={autoApprove} />);
