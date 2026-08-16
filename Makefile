# Thin wrapper for local convenience — the real implementation lives in
# .copr/Makefile, because Copr's "make srpm" SCM build method requires that
# exact path (relative to the repo root) and invokes it directly, passing
# outdir= and spec= as make variables. See .copr/Makefile for details.
#
# Local usage inside the deckery distrobox:
#   distrobox enter deckery -- make srpm

.PHONY: srpm vendor clean

srpm vendor clean:
	$(MAKE) -f .copr/Makefile $@
