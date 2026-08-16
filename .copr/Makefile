# Makefile for Copr's SCM "make srpm" build method.
#
# Copr requires this exact file (.copr/Makefile, relative to the repo root)
# for the "make_srpm" SRPM build method. It clones this repo at the tagged
# ref and invokes it like this (see
# https://docs.copr.fedorainfracloud.org/user_documentation.html#make-srpm):
#
#   make -f <cloned_repodir>/.copr/Makefile srpm outdir="<outdir>" spec="<spec_path>"
#
# This runs in a mock chroot that HAS internet access (unlike the isolated
# %build phase that follows). This target vendors Cargo dependencies fresh
# on every build and produces an SRPM — no separate CI step or pre-uploaded
# tarball needed.
#
# `outdir` and `spec` are passed in by Copr as command-line make variables
# and override the defaults below; locally, the repo-root Makefile calls
# this with no arguments, so the defaults apply.
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
VERSION := $(shell awk -F'"' '/^version/{print $$2; exit}' $(CURDIR)/Cargo.toml)
outdir  ?= $(CURDIR)/outdir
spec    ?= packaging/$(NAME).spec

.PHONY: srpm vendor builddeps clean

# Copr's srpm-generation chroot is minimal — it does NOT contain the spec's
# %build BuildRequires (those only apply to the later, isolated %build
# phase). Per the make_srpm docs, this script runs with root privileges and
# is expected to install whatever it needs itself. Guarded by `command -v
# dnf` so this is a no-op on the (Arch-based) local dev distrobox, where
# cargo/git/rpm-build are already provided as normal distrobox packages.
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
