# Training data BATCH_4 — 20 materials science domain conversations
# Tool-calling LLM fine-tune dataset
# Domains: battery, thermoelectrics, superconductors, catalysis, additive manufacturing,
#          nuclear, aerospace alloys, semiconductors, ceramics, polymers, MOFs, steel,
#          thin films, composites, topological insulators, corrosion HEA, biomaterials,
#          energy storage (Li-S), refractory metals DFT, magnetic materials

from data.training.batch_4_part1 import (
    _CONV_01,
    _CONV_02,
    _CONV_03,
    _CONV_04,
    _CONV_05,
    _CONV_06,
    _CONV_07,
)
from data.training.batch_4_part2 import (
    _CONV_08,
    _CONV_09,
    _CONV_10,
    _CONV_11,
    _CONV_12,
    _CONV_13,
    _CONV_14,
)
from data.training.batch_4_part3 import (
    _CONV_15,
    _CONV_16,
    _CONV_17,
    _CONV_18,
    _CONV_19,
    _CONV_20,
)

BATCH_4 = [
    _CONV_01,   # Battery: cathode high-voltage search (search_materials Li-Co-O)
    _CONV_02,   # Thermoelectrics: low thermal conductivity chalcogenides (semantic_search)
    _CONV_03,   # Superconductors: cuprate knowledge graph (knowledge_search + knowledge_entity)
    _CONV_04,   # Catalysis: PGM catalyst literature (literature_search)
    _CONV_05,   # Additive manufacturing: LPBF Ti-6Al-4V (literature_search)
    _CONV_06,   # Nuclear: W alloys radiation resistance (search_materials + semantic_search)
    _CONV_07,   # Aerospace: Ni-Al-Ti ternary phase diagram (calculate_phase_diagram)
    _CONV_08,   # Semiconductors: GaAs/InP/GaN band gap (predict_property × 3)
    _CONV_09,   # Ceramics: SiC structure + elastic constants (create_structure + run_workflow)
    _CONV_10,   # Polymers: dataset discovery (list_corpora domain=materials)
    _CONV_11,   # MOFs: high surface area gas storage (semantic_search)
    _CONV_12,   # Steel: Fe-C eutectoid equilibrium (calculate_equilibrium)
    _CONV_13,   # Thin films: ALD oxide patents (patent_search)
    _CONV_14,   # Composites: CFRP knowledge graph (knowledge_search + knowledge_entity)
    _CONV_15,   # Quantum materials: topological insulators small gap (search_materials × 2)
    _CONV_16,   # Corrosion: HEA corrosion resistance (search_materials + literature_search)
    _CONV_17,   # Biomaterials: hydroxyapatite Ca-P phases (search_materials)
    _CONV_18,   # Energy storage: Li-S discovery pipeline (materials_discovery)
    _CONV_19,   # Refractory metals: W-Re structure + DFT plan (create_structure + modify_structure + plan_simulations)
    _CONV_20,   # Magnetic materials: NdFeB + SmCo5 predictions (predict_property × 4)
]

assert len(BATCH_4) == 20, f"Expected 20 conversations, got {len(BATCH_4)}"
