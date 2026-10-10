#!/bin/bash
# Validate every ```mermaid block in the given Markdown files with mermaid's own
# parser. Catches the exact class of error GitHub reports at render time.
set -u
cd /tmp/mmd || exit 1
if [ "$#" -eq 0 ]; then
  set -- /home/ubuntu/dev/KoiosBase/README.md \
         /home/ubuntu/dev/KoiosBase/docs/i18n/README.*.md
fi
node check.mjs "$@"
