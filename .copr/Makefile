# Makefile for Copr's SCM "make srpm" build method.
#
# Copr requires this exact file (.copr/Makefile, relative to the repo root)
# for the "make_srpm" SRPM build method. It clones this repo at the tagged
# ref and invokes it like this (see
# https://docs.copr.fedorainfracloud.org/user_documentation.html#make-srpm):
#
#   make -f <cloned_repodir>/.copr/Makefile srpm outdir="<outdir>" spec="<spec_path>"
#
# This runs entirely on Copr's own Fedora build infrastructure, in a mock
# chroot that HAS internet access (unlike the isolated %build phase that
# follows). This target vendors Cargo dependencies fresh on every build and
# produces an SRPM — no separate CI step, no pre-uploaded tarball, and
# nothing on this machine (no distrobox, nothing) is involved in the real
# build at all.
#
# `outdir` and `spec` are passed in by Copr as command-line make variables
# and override the defaults below.
#
# Optional local sanity check (not required — the deckery distrobox is only
# for building the Rust binary during development via redeploy.sh; it is
# not needed to ship the RPM package, since Copr builds it independently):
#   make -f .copr/Makefile srpm
# Result lands in outdir/. Requires rpmbuild + cargo on whatever machine you
# run this on — if you don't have them, skip this step entirely and just
# push; the real, authoritative build happens on Copr.

NAME    := makima-deckery
VERSION := $(shell awk -F'"' '/^version/{print $$2; exit}' $(CURDIR)/Cargo.toml)
outdir  ?= $(CURDIR)/outdir
spec    ?= packaging/$(NAME).spec

.PHONY: srpm vendor builddeps clean

# Copr's srpm-generation chroot is minimal — it does NOT contain the spec's
# %build BuildRequires (those only apply to the later, isolated %build
# phase). Per the make_srpm docs, this script runs with root privileges and
# is expected to install whatever it needs itself. Guarded by `command -v
# dnf` so this is a no-op if run on a machine without dnf (e.g. locally on
# Arch) where cargo/git/rpm-build are expected to already be present.
builddeps:
	command -v dnf >/dev/null 2>&1 && ! command -v cargo >/dev/null 2>&1 \
		&& dnf -y install cargo rust git rpm-build || true

srpm: builddeps vendor
	mkdir -p $(outdir)
	git archive --prefix=$(NAME)-$(VERSION)/ -o $(outdir)/$(NAME)-$(VERSION).tar HEAD
	gzip -f $(outdir)/$(NAME)-$(VERSION).tar
	rpmbuild -bs \
		--define "_sourcedir $(outdir)" \
		--define "_srcrpmdir $(outdir)" \
		--define "_topdir $(outdir)/rpmbuild" \
		$(spec)
	@echo "SRPM written to $(outdir)"

vendor: builddeps
	mkdir -p $(outdir)
	cargo vendor > /dev/null
	tar czf $(outdir)/$(NAME)-$(VERSION)-vendor.tar.gz vendor/

clean:
	rm -rf $(outdir) vendor
