#!/usr/bin/env bash
#
# Change -> rebuild -> reload loop, via devloop (~/src/home-services/devloop).
#
#   * crates/web or crates/common change: the UI is rebuilt; open pages reload
#     themselves.
#   * crates/server or crates/common change: the server is rebuilt, and only
#     once the build succeeds is the running server replaced. A failed build
#     leaves the old one running.
#
# Duet runs at http://127.0.0.1:8484 with its data in data/ (settings in
# ~/src/home-services/devloop/apps/duet.conf), the same as from the Duet
# launcher, which also opens it in the browser. Shows the log here; Ctrl+C
# stops it.
exec "$HOME/src/home-services/devloop/devloop" dev duet "$@"
