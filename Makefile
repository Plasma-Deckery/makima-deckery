# Makefile for Copr's SCM "make srpm" build method.
#
# Copr clones this repo at the tagged ref and runs `make srpm` in a mock
# chroot that HAS internet access (unlike the isolated %build phase that
# follows). This target vendors Cargo dependencies fresh on every build and
# produces an SRPM — no separate CI step or pre-uploaded tarball needed.
# See: https://docs.copr.fedorainfracloud.org/custom_source_method.html
#
# Local usage inside the deckery distrobox (rpmbuild, cargo, git, make are
# all part of its base packages — see install.sh):
#   distrobox enter deckery -- make srpm
# Result lands in outdir/.
#
# rpmbuild here is Arch's official `extra`-repo build (RPM.org fork,
# currently RPM 6.x) — useful for local sanity checks, but not necessarily
# identical to the RPM toolchain Copr's actual Fedora build chroots use
# (typically RPM 4.x). Treat a real Copr build as the authoritative check;
# this is just for catching spec errors early.

NAME    := makima-deckery
VERSION := $(shell awk -F'"' '/^version/{print $$2; exit}' Cargo.toml)
OUTDIR  := $(CURDIR)/outdir

.PHONY: srpm vendor clean

srpm: vendor
	mkdir -p $(OUTDIR)
	git archive --prefix=$(NAME)-$(VERSION)/ -o $(OUTDIR)/$(NAME)-$(VERSION).tar HEAD
	gzip -f $(OUTDIR)/$(NAME)-$(VERSION).tar
	rpmbuild -bs \
		--define "_sourcedir $(OUTDIR)" \
		--define "_srcrpmdir $(OUTDIR)" \
		--define "_topdir $(OUTDIR)/rpmbuild" \
		packaging/$(NAME).spec
	@echo "SRPM written to $(OUTDIR)"

vendor:
	mkdir -p $(OUTDIR)
	cargo vendor > /dev/null
	tar czf $(OUTDIR)/$(NAME)-$(VERSION)-vendor.tar.gz vendor/

clean:
	rm -rf $(OUTDIR) vendor
