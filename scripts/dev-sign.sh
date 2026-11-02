#!/bin/bash
# Cargo runner: sign target/*/keel with the "Keel Dev" identity, then run it.
# The linker's ad hoc signature changes every build, so the Keychain treated
# each build as a new app and asked for the password again. A fixed identity
# and identifier (the bundle's com.keel.dev) keep "Always Allow" across builds.
# Other binaries (tests) run untouched.
bin=$1
if [ "$(basename "$bin")" = keel ] && security find-identity -p codesigning | grep -q '"Keel Dev"'; then
  codesign --force --sign "Keel Dev" --identifier com.keel.dev "$bin" 2>/dev/null
fi
exec "$@"
