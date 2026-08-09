"""Domain-owned material identity plugins used by cross-provider fusion.

Fusion never guesses a domain from a formula.  A provider or caller supplies a
``MaterialIdentity`` and the matching domain plugin turns it into a grouping
key.  This keeps unregistered domains from being accidentally merged under a
crystal-specific key.
"""
from __future__ import annotations

import json
import logging
import threading
from typing import Protocol

from app.tools.search_engine.result import MaterialIdentity

logger = logging.getLogger(__name__)

# Guards the check-then-set in register/replace: without it two threads
# racing different plugins for one domain could both pass the check and one
# would silently win -- the exact failure the refusal exists to prevent.
# (Reads in fusion_key stay lock-free: a dict read is atomic under the GIL,
# same contract as the provider-factory table.)
_PLUGINS_LOCK = threading.Lock()


class IdentityNotFusable(Exception):
    """A well-formed identity that lacks a discriminator required to merge.

    THE CONTRACT: an absent discriminator is never a value to merge on.
    A plugin raises this from ``fusion_key`` when the identity is honest but
    under-specified (e.g. a crystal with no symmetry data).  Fusion then keeps
    the record as its OWN material and records why, instead of pooling every
    record that shares the remaining attributes into one fabricated material.
    Both shipped adapters once wrote ``space_group="unknown"`` when a provider
    omitted symmetry; that sentinel keyed every polymorph of a formula into a
    single bucket, and truth-discovery then averaged e.g. the genuinely
    different band gaps of anatase/rutile/brookite TiO2 as if they were
    competing claims about one material.

    Distinct from ``ValueError`` (malformed identity / unregistered domain),
    which is an adapter bug and propagates: this is a DATA condition and must
    not fail the search.
    """


class IdentityPlugin(Protocol):
    """A domain's canonical, collision-safe fusion-key implementation."""

    domain: str

    def fusion_key(self, identity: MaterialIdentity) -> str:
        """Return a stable key, raise ``IdentityNotFusable`` when a required
        discriminator is absent, or raise ``ValueError`` when malformed."""


class CrystalIdentityPlugin:
    """Crystal identity: provider-normalized formula plus space group."""

    domain = "crystal"

    def fusion_key(self, identity: MaterialIdentity) -> str:
        if identity.representation != "formula_space_group":
            raise ValueError(
                "crystal identity representation must be 'formula_space_group'"
            )
        formula = _required_attribute(identity, "formula")
        # Providers supply normalized formulae.  Only surrounding/embedded
        # whitespace is removed here; chemical parsing or reordering would be
        # an unvalidated second identity system.
        normalized_formula = "".join(formula.split())
        if not normalized_formula:
            raise ValueError("crystal identity requires a non-empty formula")
        space_group = (identity.attributes.get("space_group") or "").strip()
        # An absent discriminator must never be a value to merge on.  The
        # legacy "unknown" sentinel is rejected alongside a missing/empty
        # attribute so no future adapter can reintroduce the polymorph-merge
        # bug by writing a placeholder instead of omitting the attribute.
        if not space_group or space_group.lower() == "unknown":
            raise IdentityNotFusable(
                f"no symmetry data: cannot distinguish polymorphs of "
                f"{normalized_formula!r} without a space group"
            )
        return _key(
            identity.domain,
            identity.representation,
            normalized_formula,
            space_group,
        )


class PolymerIdentityPlugin:
    """Polymer identity using the supplied structured representation anchor.

    ``repeat_unit`` is the preferred polymer anchor.  ``monomer`` and
    ``smiles`` are accepted because they are the other representations used by
    the polymer domain.  Values are compared exactly after trimming only:
    without an RDKit-backed canonicalizer, treating differently written
    structures as equivalent could merge chemically different polymers.
    """

    domain = "polymer"
    _REPRESENTATIONS = frozenset({"repeat_unit", "monomer", "smiles"})

    def fusion_key(self, identity: MaterialIdentity) -> str:
        representation = identity.representation
        if representation not in self._REPRESENTATIONS:
            expected = ", ".join(sorted(self._REPRESENTATIONS))
            raise ValueError(
                f"unsupported polymer identity representation {representation!r}; "
                f"expected one of {expected}"
            )
        anchor = _required_attribute(identity, representation)
        return _key(identity.domain, representation, anchor)


def _required_attribute(identity: MaterialIdentity, name: str) -> str:
    value = identity.attributes.get(name)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(
            f"{identity.domain} identity representation {identity.representation!r} "
            f"requires a non-empty {name!r} attribute"
        )
    return value.strip()


def _key(*parts: str) -> str:
    """Encode parts structurally so delimiters in identities cannot collide."""
    return json.dumps(parts, ensure_ascii=True, separators=(",", ":"))


_IDENTITY_PLUGINS: dict[str, IdentityPlugin] = {
    CrystalIdentityPlugin.domain: CrystalIdentityPlugin(),
    PolymerIdentityPlugin.domain: PolymerIdentityPlugin(),
}


def _validated_domain(plugin: IdentityPlugin) -> str:
    domain = getattr(plugin, "domain", "")
    if not isinstance(domain, str) or not domain.strip():
        raise ValueError("identity plugin must declare a non-empty domain")
    if not callable(getattr(plugin, "fusion_key", None)):
        raise ValueError("identity plugin must define fusion_key(identity)")
    return domain


def register_identity_plugin(plugin: IdentityPlugin) -> None:
    """Register a domain-owned identity plugin for an explicitly named domain.

    Registration is deliberately explicit: a material labelled with an
    unregistered domain raises from ``fusion_key`` instead of falling back to a
    crystal formula key.

    Re-registering the SAME plugin object is a no-op (module re-imports).
    Registering a DIFFERENT plugin for a taken domain raises, because two
    plugins for one domain would mean one silently wins; taking over a
    domain is a deliberate act with its own call:
    ``replace_identity_plugin``.
    """
    domain = _validated_domain(plugin)
    with _PLUGINS_LOCK:  # check-then-set must be atomic across threads
        existing = _IDENTITY_PLUGINS.get(domain)
        if existing is not None:
            if existing is plugin:
                return  # idempotent: same plugin, same domain
            raise ValueError(
                f"identity plugin already registered for domain {domain!r}; "
                "use replace_identity_plugin to swap it deliberately"
            )
        _IDENTITY_PLUGINS[domain] = plugin


def replace_identity_plugin(plugin: IdentityPlugin) -> IdentityPlugin:
    """Deliberately swap the plugin for an ALREADY-registered domain.

    The two-call contract, shared by every adapter plane: ``register``
    refuses a taken domain (an accidental collision fails loudly),
    ``replace`` refuses a FREE domain (a typo'd domain cannot silently ADD a
    plugin while the one you meant to displace keeps running).

    Returns the displaced plugin -- hand it back to this function to restore
    the original -- and logs what was displaced.
    """
    domain = _validated_domain(plugin)
    with _PLUGINS_LOCK:  # check-then-set must be atomic across threads
        displaced = _IDENTITY_PLUGINS.get(domain)
        if displaced is None:
            raise ValueError(
                f"no identity plugin registered for domain {domain!r} to replace; "
                "use register_identity_plugin to add a new one"
            )
        _IDENTITY_PLUGINS[domain] = plugin
    logger.info(
        "identity plugin for domain %r replaced: %r displaced by %r",
        domain, displaced, plugin,
    )
    return displaced


def fusion_key(identity: MaterialIdentity) -> str:
    """Return the domain plugin's key; reject unregistered domains honestly."""
    plugin = _IDENTITY_PLUGINS.get(identity.domain)
    if plugin is None:
        raise ValueError(f"unknown material identity domain {identity.domain!r}")
    return plugin.fusion_key(identity)
