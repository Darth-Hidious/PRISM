"""Search providers — pluggable data source interface.

Adding a new API family is ONE new adapter module and ZERO edits to
registries, dispatch, or enums:

1. Drop ``foo.py`` in this package. Subclass ``base.Provider`` and call
   ``registry.register_provider_factory("foo", FooProvider)`` at module
   import time. ``registry.load_provider_plugins()`` (run automatically on
   the first ``ProviderRegistry.from_endpoints``) imports every public
   module here, alphabetically, so the registration executes. Modules whose
   names start with ``_`` or ``test`` are never imported.
2. Or, outside the package: list the adapter under ``adapter_modules:`` in
   ``~/.prism/providers.yaml`` — importable module names or absolute paths
   to ``.py`` files.

A broken adapter is skipped with a WARNING naming the module and error; it
never takes down the other providers. This file must stay side-effect-free
(no submodule imports) so the package can never import-cycle with them.
"""
