#!/bin/sh
# Replace the shell so the scheduler's forwarded SIGUSR1 reaches PRISM.
exec "${PRISM_BIN:-prism}" campaign batch-entrypoint
