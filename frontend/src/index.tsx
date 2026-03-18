#!/usr/bin/env bun
import React from "react";
import { render } from "ink";
import { App } from "./app.js";

const args = process.argv.slice(2);
let pythonPath = "python3";
let backendBin: string | undefined;
let autoApprove = false;
let resume: string | undefined;

for (let i = 0; i < args.length; i++) {
  if (args[i] === "--python" && args[i + 1]) {
    pythonPath = args[i + 1];
    i++;
  }
  if (args[i] === "--backend-bin" && args[i + 1]) {
    backendBin = args[i + 1];
    i++;
  }
  if (args[i] === "--auto-approve") {
    autoApprove = true;
  }
  if (args[i] === "--resume" && args[i + 1]) {
    resume = args[i + 1];
    i++;
  }
}

render(
  <App
    pythonPath={pythonPath}
    backendBin={backendBin}
    autoApprove={autoApprove}
    resume={resume}
  />,
);
