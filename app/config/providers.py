# app/config/providers.py

FALLBACK_PROVIDERS = [
    {
        "id": "mp",
        "name": "Materials Project",
        "description": "The Materials Project is a core program of the Lawrence Berkeley National Laboratory (LBNL) Materials Science Division.",
        "base_url": "https://optimade.materialsproject.org/"
    },
    {
        "id": "oqmd",
        "name": "Open Quantum Materials Database",
        "description": "The OQMD is a database of DFT calculated thermodynamic and structural properties of inorganic compounds.",
        "base_url": "https://oqmd.org/optimade/"
    },
    {
        "id": "cod",
        "name": "Crystallography Open Database",
        "description": "An open-access collection of crystal structures of organic, inorganic, metal-organic compounds and minerals.",
        "base_url": "https://www.crystallography.net/cod/optimade"
    },
    {
        "id": "aflow",
        "name": "AFLOW",
        "description": "Automatic FLOW for Materials Discovery.",
        "base_url": "https://aflow.org/API/optimade/"
    },
    {
        "id": "jarvis",
        "name": "JARVIS",
        "description": "Joint Automated Repository for Various Integrated Simulations (JARVIS) is a repository for properties of materials from classical and quantum simulations.",
        "base_url": "https://jarvis.nist.gov/optimade/jarvisdft/"
    },
    {
        "id": "mcloud",
        "name": "Materials Cloud",
        "description": "A platform for Open Science built for seamless sharing of resources for computational science.",
        "base_url": "https://optimade.materialscloud.org/"
    }
] 