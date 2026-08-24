"""Operation handlers.

Importing this package populates :data:`blender_extension.dispatcher.HANDLERS`.
Every handler is registered by an explicit decorator with a literal name, so
the set of reachable operations is fixed at import time and visible by reading
the source.
"""

from __future__ import annotations

from .. import dispatcher

# Import for side effects: each module registers its handlers on import.
# Listed explicitly rather than discovered, so an accidental file in this
# directory cannot add an operation.
from . import animation  # noqa: F401
from . import batch  # noqa: F401
from . import camera  # noqa: F401
from . import collection  # noqa: F401
from . import diagnostics  # noqa: F401
from . import geometry_nodes  # noqa: F401
from . import io  # noqa: F401
from . import light  # noqa: F401
from . import material  # noqa: F401
from . import mesh  # noqa: F401
from . import modifier  # noqa: F401
from . import object  # noqa: F401
from . import render  # noqa: F401
from . import rigging  # noqa: F401
from . import scene  # noqa: F401
from . import scene_surface  # noqa: F401
from . import shader  # noqa: F401
from . import texture  # noqa: F401
from . import transaction  # noqa: F401
from . import utilities  # noqa: F401
from . import uv  # noqa: F401

__all__ = ["operation_names", "operation_count"]


def operation_names() -> list[str]:
    return sorted(dispatcher.HANDLERS)


def operation_count() -> int:
    return len(dispatcher.HANDLERS)
