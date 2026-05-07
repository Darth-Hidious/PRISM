# Training data batch 4 — part 1 (conversations 1-7)
# Materials science domains: battery, thermoelectrics, superconductors,
# catalysis, additive manufacturing, nuclear, aerospace

_CONV_01 = {
    "messages": [
        {
            "role": "system",
            "content": (
                "You are PRISM, an AI-native materials discovery assistant built by MARC27. "
                "You have access to federated materials databases including the Materials Project, "
                "OPTIMADE providers, and the MARC27 knowledge graph. "
                "When answering, reason step by step inside <think> tags before making tool calls."
            ),
        },
        {
            "role": "user",
            "content": "Find cathode materials that could work at high voltage, above 4 V vs Li/Li+. I need oxide or phosphate candidates.",
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "The user wants cathode materials for lithium-ion batteries with high operating voltage "
                "(>4 V vs Li/Li+). Classic high-voltage cathodes include LiCoO2 (~3.9 V), LiNiO2, "
                "LiMn2O4 spinel (~4.1 V), LiMnPO4 (~4.1 V), LiCoPO4 (~4.8 V), LiFePO4 (~3.4 V, too low), "
                "and Li2MnO3-based layered oxides. I should search for materials containing Li and transition "
                "metals (Co, Mn, Ni) in oxide/phosphate systems. The band_gap constraint doesn't directly map "
                "to voltage, but I can search by element sets. I'll use search_materials with elements "
                "including Li, Co or Mn, and O — targeting layered oxides. Then follow up with a semantic "
                "search for context.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "search_materials",
                        "arguments": {
                            "elements": ["Li", "Co", "O"],
                            "crystal_system": "trigonal",
                            "limit": 15,
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "search_materials",
            "tool_call_id": "call_1",
            "content": (
                '{"results": ['
                '{"id": "mp-24850", "formula": "LiCoO2", "elements": ["Co", "Li", "O"], '
                '"band_gap": {"value": 2.1, "unit": "eV", "source": "mp"}, '
                '"space_group": "R-3m", "formation_energy": {"value": -3.21, "unit": "eV/atom"}, '
                '"sources": ["mp"]},'
                '{"id": "mp-19399", "formula": "LiMn2O4", "elements": ["Li", "Mn", "O"], '
                '"band_gap": {"value": 0.0, "unit": "eV", "source": "mp"}, '
                '"space_group": "Fd-3m", "formation_energy": {"value": -2.89, "unit": "eV/atom"}, '
                '"sources": ["mp"]},'
                '{"id": "mp-149427", "formula": "LiNiO2", "elements": ["Li", "Ni", "O"], '
                '"band_gap": {"value": 0.0, "unit": "eV", "source": "mp"}, '
                '"space_group": "R-3m", "formation_energy": {"value": -2.97, "unit": "eV/atom"}, '
                '"sources": ["mp"]},'
                '{"id": "mp-758082", "formula": "LiCoPO4", "elements": ["Co", "Li", "O", "P"], '
                '"band_gap": {"value": 3.8, "unit": "eV", "source": "mp"}, '
                '"space_group": "Pnma", "formation_energy": {"value": -4.12, "unit": "eV/atom"}, '
                '"sources": ["mp"]},'
                '{"id": "mp-25685", "formula": "LiMnPO4", "elements": ["Li", "Mn", "O", "P"], '
                '"band_gap": {"value": 3.5, "unit": "eV", "source": "mp"}, '
                '"space_group": "Pnma", "formation_energy": {"value": -3.98, "unit": "eV/atom"}, '
                '"sources": ["mp"]}'
                '], "count": 5, "search_time_ms": 312, "cached": false, "warnings": [], '
                '"providers_queried": [{"id": "mp", "status": "ok", "results": 5}]}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "I got good results. Now I can interpret them for high-voltage cathode suitability:\n"
                "- LiCoO2 (mp-24850): R-3m layered oxide, the classic cathode, ~3.9-4.2 V practical range. "
                "  Band gap 2.1 eV.\n"
                "- LiMn2O4 (mp-19399): spinel structure, ~4.0-4.1 V vs Li/Li+. Metallic per MP.\n"
                "- LiNiO2 (mp-149427): layered, ~3.6-4.2 V, structural instability issues.\n"
                "- LiCoPO4 (mp-758082): olivine phosphate, ~4.8 V — genuinely high voltage. Large band gap "
                "  (3.8 eV) means poor electronic conductivity, needs carbon coating.\n"
                "- LiMnPO4 (mp-25685): olivine, ~4.1 V, large band gap 3.5 eV, similar conductivity issue.\n"
                "LiCoPO4 is the standout high-voltage candidate at ~4.8 V. I should mention this clearly.\n"
                "</think>\n\n"
                "Here are the top high-voltage cathode candidates from the federated search:\n\n"
                "| Formula | Structure | Est. Voltage vs Li/Li+ | Band Gap | Notes |\n"
                "|---------|-----------|------------------------|----------|-------|\n"
                "| **LiCoPO4** (mp-758082) | Olivine (Pnma) | ~4.8 V | 3.8 eV | Highest voltage; poor conductivity — needs carbon coating |\n"
                "| **LiMnPO4** (mp-25685) | Olivine (Pnma) | ~4.1 V | 3.5 eV | Phosphate stability; kinetically sluggish |\n"
                "| **LiMn2O4** (mp-19399) | Spinel (Fd-3m) | ~4.0–4.1 V | Metal | Mature technology; Mn dissolution at high T |\n"
                "| **LiCoO2** (mp-24850) | Layered (R-3m) | ~3.9–4.2 V | 2.1 eV | Industrial standard; Co cost and safety tradeoffs |\n"
                "| **LiNiO2** (mp-149427) | Layered (R-3m) | ~3.6–4.2 V | Metal | High capacity but thermally unstable |\n\n"
                "**Recommendations:**\n"
                "- For maximum voltage (>4.5 V), **LiCoPO4** is the primary target. Its 4.8 V plateau exceeds "
                "conventional electrolyte stability windows — pair with ionic liquid or solid electrolytes.\n"
                "- **LiMnPO4** at 4.1 V is safer and Mn is abundant; the kinetic limitation is manageable with "
                "nanostructuring.\n"
                "- Spinel **LiMn2O4** is the most mature option if 4.1 V suffices.\n\n"
                "Want me to run a literature search on LiCoPO4 electrolyte compatibility, or fetch DFT-computed "
                "voltage profiles from the Materials Project?"
            ),
        },
    ]
}

_CONV_02 = {
    "messages": [
        {
            "role": "system",
            "content": (
                "You are PRISM, an AI materials discovery platform. You assist researchers in finding "
                "materials for energy conversion applications. Use tools to search databases and provide "
                "evidence-based recommendations. Think through your reasoning in <think> tags."
            ),
        },
        {
            "role": "user",
            "content": "Search for materials with low thermal conductivity suitable for thermoelectric applications. I'm particularly interested in chalcogenides.",
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Thermoelectric materials need: high Seebeck coefficient, high electrical conductivity, and "
                "LOW thermal conductivity (zT = S²σT/κ). Classic thermoelectrics are chalcogenides: "
                "Bi2Te3 (room temp), PbTe (mid-temp), GeTe, SnSe (record zT~2.6), Cu2Se, Ag2Se, "
                "and half-Heuslers. The user specifically wants chalcogenides (S, Se, Te compounds). "
                "Low thermal conductivity in DFT databases isn't always stored directly, but I can:\n"
                "1. Use semantic_search with a thermoelectric/low thermal conductivity query to find relevant "
                "   materials from the knowledge graph.\n"
                "2. Use search_materials to find chalcogenide compounds (elements including Te or Se or S).\n"
                "Let me do a semantic search first as it will capture the conceptual match better.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "semantic_search",
                        "arguments": {
                            "query": "chalcogenide materials low thermal conductivity high thermoelectric figure of merit zT",
                            "limit": 10,
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "semantic_search",
            "tool_call_id": "call_1",
            "content": (
                '{"results": ['
                '{"score": 0.94, "id": "doc_8821", "title": "SnSe single crystal thermoelectric", '
                '"snippet": "SnSe achieves a record zT of 2.6 along the b-axis at 923 K due to '
                'ultralow lattice thermal conductivity (0.23 W/mK) arising from strong anharmonic bonding.", '
                '"source": "marc27_pgvector"},'
                '{"score": 0.91, "id": "doc_3344", "title": "Bi2Te3-based alloys", '
                '"snippet": "Bi2Te3 and its solid solutions (Bi2-xSbxTe3, Bi2Te3-xSex) are the benchmark '
                'room-temperature thermoelectrics with zT~1 at 300 K.", "source": "marc27_pgvector"},'
                '{"score": 0.88, "id": "doc_5512", "title": "Cu2Se fast-ion conductor thermoelectric", '
                '"snippet": "Cu2Se exhibits liquid-like Cu sublattice above 400 K, yielding extremely low '
                'thermal conductivity (~0.4 W/mK) and zT~1.5.", "source": "marc27_pgvector"},'
                '{"score": 0.85, "id": "doc_7731", "title": "PbTe thermoelectrics band convergence", '
                '"snippet": "Band convergence in Na-doped PbTe yields zT~1.8 at 850 K; low κ from resonant '
                'bonding in the rocksalt structure.", "source": "marc27_pgvector"},'
                '{"score": 0.82, "id": "doc_6603", "title": "AgSbTe2 low thermal conductivity", '
                '"snippet": "AgSbTe2 has κ < 0.5 W/mK due to mass fluctuation scattering and local '
                'structural disorder, making it a promising mid-temperature thermoelectric.", '
                '"source": "marc27_pgvector"}'
                '], "count": 5, "query": "chalcogenide materials low thermal conductivity high thermoelectric figure of merit zT", '
                '"source": "marc27_pgvector"}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Excellent semantic search results. I have five strong chalcogenide thermoelectric systems "
                "with documented low thermal conductivity. Let me also do a quick search_materials for "
                "SnSe and Bi2Te3 to get DFT properties.\n"
                "Actually, the question is well-answered by the semantic search. Let me summarize clearly "
                "with the key metrics.\n"
                "</think>\n\n"
                "Here are the top chalcogenide candidates for thermoelectric applications, ranked by "
                "thermoelectric figure of merit (zT):\n\n"
                "| Material | κ (W/mK) | Peak zT | Temperature Range | Key Mechanism |\n"
                "|----------|----------|---------|-------------------|---------------|\n"
                "| **SnSe** | 0.23 (b-axis) | ~2.6 | 700–923 K | Strong phonon anharmonicity, layered structure |\n"
                "| **Cu2Se** | ~0.4 | ~1.5 | 400–700 K | Liquid-like Cu sublattice (phonon-glass electron-crystal) |\n"
                "| **Na-doped PbTe** | ~0.8 | ~1.8 | 600–850 K | Band convergence + resonant bonding |\n"
                "| **AgSbTe2** | <0.5 | ~1.2 | 500–700 K | Mass fluctuation + structural disorder scattering |\n"
                "| **Bi2Te3 alloys** | ~1.0 | ~1.0 | 250–400 K | Established room-temperature benchmark |\n\n"
                "**Key insights:**\n"
                "- **SnSe** holds the record zT (~2.6) in single-crystal form; polycrystalline SnSe reaches "
                "~1.0–1.4 — still exceptional for a simple binary compound.\n"
                "- **Cu2Se** and **AgSbTe2** achieve low κ through structural disorder rather than complex "
                "chemistry — synthetically accessible.\n"
                "- **Bi2Te3**-based alloys remain the only commercially deployed thermoelectrics (Peltier coolers).\n\n"
                "Want me to fetch DFT-computed formation energies and band structures for any of these from "
                "the Materials Project, or search the literature for doping strategies to further optimize zT?"
            ),
        },
    ]
}

_CONV_03 = {
    "messages": [
        {
            "role": "system",
            "content": (
                "You are PRISM, an AI assistant for materials science research at MARC27. "
                "You have access to the MARC27 knowledge graph with 200K+ nodes covering materials, "
                "properties, synthesis methods, and publications. Reason before acting."
            ),
        },
        {
            "role": "user",
            "content": "What does the knowledge graph know about high-Tc cuprate superconductors? I want to understand which compounds and properties are represented.",
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "The user wants to explore knowledge graph coverage of cuprate superconductors. Cuprates are "
                "copper-oxide based superconductors with high critical temperature (Tc). Key compounds:\n"
                "- YBCO (YBa2Cu3O7), Tc~92 K\n"
                "- BSCCO (Bi2Sr2CaCu2O8), Tc~110 K\n"
                "- TBCCO (Tl2Ba2CaCu2O8), Tc~110 K\n"
                "- HgBa2Ca2Cu3O8 (Hg-1223), Tc~133 K (ambient pressure record)\n"
                "- La2-xSrxCuO4 (LSCO), Tc~38 K\n"
                "I should use knowledge_search first with 'cuprate superconductor' to see what entities "
                "are in the graph, then use knowledge_entity on a key compound to explore relationships.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "knowledge_search",
                        "arguments": {
                            "term": "cuprate superconductor high-Tc",
                            "limit": 20,
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "knowledge_search",
            "tool_call_id": "call_1",
            "content": (
                '{"results": ['
                '{"id": "ent_2201", "name": "YBa2Cu3O7", "type": "Material", '
                '"labels": ["cuprate", "superconductor", "perovskite-related", "YBCO"]},'
                '{"id": "ent_2202", "name": "Bi2Sr2CaCu2O8", "type": "Material", '
                '"labels": ["cuprate", "superconductor", "BSCCO", "Bi-2212"]},'
                '{"id": "ent_2203", "name": "HgBa2Ca2Cu3O8", "type": "Material", '
                '"labels": ["cuprate", "superconductor", "highest-Tc-ambient"]},'
                '{"id": "ent_2204", "name": "La2-xSrxCuO4", "type": "Material", '
                '"labels": ["cuprate", "superconductor", "LSCO", "doped-Mott-insulator"]},'
                '{"id": "ent_3301", "name": "Critical Temperature", "type": "Property", '
                '"labels": ["Tc", "superconducting transition", "quantum phase transition"]},'
                '{"id": "ent_3302", "name": "d-wave pairing symmetry", "type": "Mechanism", '
                '"labels": ["cuprate pairing", "unconventional superconductor"]},'
                '{"id": "ent_3303", "name": "CuO2 plane", "type": "StructuralFeature", '
                '"labels": ["cuprate building block", "correlated electrons"]},'
                '{"id": "ent_4401", "name": "Bednorz and Muller 1986", "type": "Publication", '
                '"labels": ["Nobel Prize", "high-Tc discovery", "cuprate"]},'
                '{"id": "ent_2205", "name": "Tl2Ba2Ca2Cu3O10", "type": "Material", '
                '"labels": ["cuprate", "superconductor", "Tc-125K", "Tl-2223"]}'
                '], "count": 9, "query": "cuprate superconductor high-Tc", "source": "marc27_knowledge_graph"}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Good — the knowledge graph has 9 entities covering cuprate superconductors: 5 materials, "
                "2 properties/mechanisms, 1 structural feature, 1 publication. Let me now call knowledge_entity "
                "on YBCO (ent_2201) to see what relationships it has — this will show which properties, "
                "synthesis methods, and papers are linked to it.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_2",
                    "type": "function",
                    "function": {
                        "name": "knowledge_entity",
                        "arguments": {
                            "name": "YBa2Cu3O7",
                            "limit": 15,
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "knowledge_entity",
            "tool_call_id": "call_2",
            "content": (
                '{"entity": {"id": "ent_2201", "name": "YBa2Cu3O7", "type": "Material", '
                '"labels": ["cuprate", "YBCO", "superconductor"]}, '
                '"neighbors": {'
                '"has_property": ['
                '{"name": "Critical Temperature", "value": "92 K", "source": "experiment"},'
                '{"name": "Upper Critical Field Hc2", "value": "~150 T (ab-plane)"},'
                '{"name": "Coherence Length", "value": "1.6 nm (ab), 0.3 nm (c)"},'
                '{"name": "London Penetration Depth", "value": "~150 nm"}'
                '],'
                '"synthesized_by": ['
                '{"name": "solid-state sintering", "conditions": "900°C, oxygen atmosphere"},'
                '{"name": "pulsed laser deposition", "conditions": "thin film growth on SrTiO3"}'
                '],'
                '"crystal_structure": {"space_group": "Pmmm", "structure_type": "orthorhombic perovskite-related", "CuO2_planes": 2},'
                '"cited_in": ['
                '{"name": "Wu et al. 1987 PRL", "doi": "10.1103/PhysRevLett.58.908"},'
                '{"name": "Cava et al. 1987 PRL", "doi": "10.1103/PhysRevLett.58.1676"}'
                '],'
                '"related_materials": ['
                '{"name": "GdBa2Cu3O7", "relation": "isostructural, Tc~94 K"},'
                '{"name": "YBa2Cu4O8", "relation": "double-chain variant, Tc~82 K"}'
                ']'
                '}, "source": "marc27_knowledge_graph"}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Excellent — I now have a comprehensive picture. Let me compile the full summary for the user.\n"
                "</think>\n\n"
                "The MARC27 knowledge graph has solid coverage of cuprate superconductors. Here is what it contains:\n\n"
                "**Cuprate Materials in the Graph**\n\n"
                "| Compound | Common Name | Tc | Notes |\n"
                "|----------|-------------|-----|-------|\n"
                "| YBa2Cu3O7 | YBCO / Y-123 | 92 K | 2 CuO2 planes, Pmmm symmetry |\n"
                "| Bi2Sr2CaCu2O8 | BSCCO / Bi-2212 | ~110 K | Naturally cleaves — ideal for ARPES |\n"
                "| HgBa2Ca2Cu3O8 | Hg-1223 | ~133 K | Highest ambient-pressure Tc known |\n"
                "| Tl2Ba2Ca2Cu3O10 | Tl-2223 | ~125 K | Three CuO2 planes |\n"
                "| La2-xSrxCuO4 | LSCO | ~38 K | Prototype doped Mott insulator; phase diagram benchmark |\n\n"
                "**Properties Represented for YBCO**\n"
                "- Critical temperature Tc = 92 K (experimentally confirmed)\n"
                "- Upper critical field Hc2 ≈ 150 T (ab-plane)\n"
                "- Coherence length: 1.6 nm (ab), 0.3 nm (c-axis)\n"
                "- London penetration depth: ~150 nm\n\n"
                "**Other Entities**\n"
                "- Mechanism: *d-wave pairing symmetry* (unconventional superconductor)\n"
                "- Structural feature: *CuO2 planes* (the defining building block)\n"
                "- Publication: Bednorz & Müller 1986 (Nobel Prize discovery)\n\n"
                "**Coverage gaps to note:** The graph does not yet contain Tc data for pressure-tuned "
                "cuprates (HgBa2CuO4+δ under pressure reaches ~164 K), and doping-dependent phase diagrams "
                "(pseudogap, strange metal) are represented only conceptually.\n\n"
                "Want me to search for recent literature on any specific cuprate, or explore the graph paths "
                "between cuprate materials and synthesis methods?"
            ),
        },
    ]
}

_CONV_04 = {
    "messages": [
        {
            "role": "system",
            "content": (
                "You are PRISM, an AI-native materials discovery platform for professional researchers. "
                "You have literature search capabilities spanning arXiv and Semantic Scholar. "
                "Always reason through your search strategy before executing."
            ),
        },
        {
            "role": "user",
            "content": "Find key papers on platinum group metal catalysts — specifically Pd, Pt, Rh, Ru used in heterogeneous catalysis.",
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "The user wants literature on PGM (platinum group metals) heterogeneous catalysts. "
                "Pd, Pt, Rh, Ru are the main PGMs. Applications include:\n"
                "- Automotive catalytic converters (Pt, Pd, Rh)\n"
                "- CO oxidation (Pt, Pd)\n"
                "- Selective hydrogenation (Pd)\n"
                "- Steam reforming (Ru, Rh)\n"
                "- NH3 synthesis (Ru-based)\n"
                "I should use literature_search with a targeted query. I'll include 'heterogeneous catalysis' "
                "and PGM-specific terms. Let me run a single broad search.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "literature_search",
                        "arguments": {
                            "query": "platinum group metal Pd Pt Rh Ru heterogeneous catalysis surface reaction mechanism",
                            "max_results": 12,
                            "sources": ["semantic_scholar", "arxiv"],
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "literature_search",
            "tool_call_id": "call_1",
            "content": (
                '{"results": ['
                '{"title": "Single-atom catalysis of CO oxidation using Pt1/FeOx", '
                '"authors": ["B. Qiao", "A. Wang", "X. Yang"], "year": 2011, '
                '"journal": "Nature Chemistry", "citations": 4821, '
                '"doi": "10.1038/nchem.1095", '
                '"abstract": "Isolated Pt atoms anchored on FeOx achieve high CO oxidation turnover with '
                'minimal Pt loading, demonstrating single-atom catalysis."},'
                '{"title": "Palladium-catalyzed cross-coupling reactions: a historical perspective", '
                '"authors": ["A. Suzuki"], "year": 2011, '
                '"journal": "Angewandte Chemie", "citations": 3200, '
                '"abstract": "Overview of Pd-catalyzed C-C coupling reactions and mechanistic insights."},'
                '{"title": "Ruthenium nanoparticles for ammonia synthesis: a computational study", '
                '"authors": ["K. Honkala", "A. Hellman", "J.K. Norskov"], "year": 2005, '
                '"journal": "Science", "citations": 2870, '
                '"doi": "10.1126/science.1106435", '
                '"abstract": "DFT calculations show Ru step sites are active for N2 dissociation; '
                'microkinetic model predicts TOF in agreement with experiment."},'
                '{"title": "Rhodium-based three-way catalysts: oxygen storage and NOx reduction", '
                '"authors": ["R.J. Farrauto", "C.H. Bartholomew"], "year": 2006, '
                '"journal": "Catalysis Today", "citations": 1540, '
                '"abstract": "Rh facilitates NOx reduction while Pt/Pd handle CO and hydrocarbon oxidation '
                'in automotive three-way catalysts."},'
                '{"title": "Surface science of Pt(111): CO adsorption and oxidation", '
                '"authors": ["T. Engel", "G. Ertl"], "year": 1979, '
                '"journal": "Advances in Catalysis", "citations": 3900, '
                '"abstract": "Landmark study of CO oxidation on Pt(111) via Langmuir-Hinshelwood mechanism."},'
                '{"title": "High-entropy alloy electrocatalysts for oxygen reduction", '
                '"authors": ["Y. Yao", "Z. Huang", "P. Xie"], "year": 2022, '
                '"journal": "Science", "citations": 890, '
                '"abstract": "Multi-principal-element alloys incorporating Pt outperform pure Pt for ORR."}'
                '], "count": 6, "query": "platinum group metal Pd Pt Rh Ru heterogeneous catalysis"}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Good set of papers covering all four PGMs requested. Let me organize them by element and application.\n"
                "</think>\n\n"
                "Here are the key papers found across PGM catalysis:\n\n"
                "**Platinum (Pt)**\n"
                "- Qiao et al. (2011, *Nature Chemistry*, 4821 citations): Single-atom Pt1/FeOx for CO oxidation — "
                "defines the single-atom catalysis paradigm.\n"
                "- Engel & Ertl (1979, *Adv. Catalysis*, 3900 citations): Landmark Pt(111) CO oxidation "
                "via Langmuir-Hinshelwood — still a foundational reference for surface kinetics.\n\n"
                "**Palladium (Pd)**\n"
                "- Suzuki (2011, *Angew. Chemie*, 3200 citations): Historical perspective on Pd-catalyzed "
                "cross-coupling (Nobel Prize work). Essential for homogeneous/heterogeneous boundary research.\n\n"
                "**Ruthenium (Ru)**\n"
                "- Honkala, Hellman & Nørskov (2005, *Science*, 2870 citations): DFT + microkinetics for "
                "NH3 synthesis on Ru — a model study demonstrating predictive catalysis theory.\n\n"
                "**Rhodium (Rh)**\n"
                "- Farrauto & Bartholomew (2006, *Catalysis Today*, 1540 citations): Three-way catalyst "
                "overview — Rh's unique role in NOx reduction that Pt/Pd cannot replicate.\n\n"
                "**Cross-cutting**\n"
                "- Yao et al. (2022, *Science*, 890 citations): High-entropy alloys with Pt for ORR — "
                "relevant if you are exploring reduced PGM loading strategies.\n\n"
                "Want me to narrow down to a specific PGM, application (e.g., hydrogenation, fuel cells), "
                "or search for more recent papers from 2023–2026?"
            ),
        },
    ]
}

_CONV_05 = {
    "messages": [
        {
            "role": "system",
            "content": (
                "You are PRISM, a materials discovery AI assistant. You have access to literature search "
                "tools covering arXiv and Semantic Scholar. You support researchers working on advanced "
                "manufacturing and materials processing."
            ),
        },
        {
            "role": "user",
            "content": "Search for papers on laser powder bed fusion of titanium alloys, especially Ti-6Al-4V microstructure and defect formation.",
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "The user is asking about additive manufacturing — specifically Laser Powder Bed Fusion (LPBF, "
                "also called SLM: selective laser melting) of titanium alloys. Ti-6Al-4V is the dominant "
                "Ti alloy used in AM. Key research topics include:\n"
                "- Microstructure: columnar prior-β grains, martensitic α' phase, acicular α+β after HIP/anneal\n"
                "- Defects: porosity (lack of fusion, keyhole), cracking, residual stress\n"
                "- Process parameters: laser power, scan speed, hatch spacing, layer thickness\n"
                "I'll search for LPBF Ti-6Al-4V with microstructure and defect keywords.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "literature_search",
                        "arguments": {
                            "query": "laser powder bed fusion Ti-6Al-4V microstructure porosity defect selective laser melting",
                            "max_results": 10,
                            "sources": ["semantic_scholar", "arxiv"],
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "literature_search",
            "tool_call_id": "call_1",
            "content": (
                '{"results": ['
                '{"title": "Microstructure and mechanical properties of Ti-6Al-4V produced by selective laser melting", '
                '"authors": ["L.E. Murr", "S.M. Gaytan", "D.A. Ramirez"], "year": 2012, '
                '"journal": "Journal of Materials Research and Technology", "citations": 1890, '
                '"abstract": "SLM Ti-6Al-4V exhibits columnar prior-β grains with martensitic α\' phase; '
                'post-HIP anneal converts to equiaxed α+β improving ductility."},'
                '{"title": "Defect generation in laser powder bed fusion: a review", '
                '"authors": ["C. Zhao", "K. Fezzaa", "R.W. Cunningham"], "year": 2017, '
                '"journal": "Scientific Reports", "citations": 2140, '
                '"abstract": "In-situ synchrotron X-ray imaging reveals keyhole and lack-of-fusion pore "  '
                '"formation mechanisms during LPBF of Ti-6Al-4V."},'
                '{"title": "Effect of process parameters on porosity in LPBF Ti-6Al-4V", '
                '"authors": ["D. Greitemeier", "F. Palm", "F. Syassen"], "year": 2017, '
                '"journal": "International Journal of Fatigue", "citations": 870, '
                '"abstract": "Optimized volumetric energy density (55-65 J/mm3) minimizes porosity to <0.1% '
                'in LPBF Ti-6Al-4V; surface roughness remains a fatigue initiation site."},'
                '{"title": "Residual stress in additively manufactured titanium: neutron diffraction study", '
                '"authors": ["M. Shiomi", "K. Osakada", "K. Nakamura"], "year": 2004, '
                '"journal": "CIRP Annals", "citations": 1230, '
                '"abstract": "High residual tensile stresses up to 400 MPa measured in SLM Ti parts; '
                'stress relief at 650°C for 3h reduces residual stress by 70%."},'
                '{"title": "Columnar to equiaxed grain transition in LPBF of Ti-6Al-4V via scanning strategy", '
                '"authors": ["T. DebRoy", "H.L. Wei", "J.S. Zuback"], "year": 2021, '
                '"journal": "Progress in Materials Science", "citations": 3200, '
                '"abstract": "Rotating scan strategy and modified thermal gradients promote columnar-to-equiaxed '
                'transition, improving texture-induced anisotropy in LPBF Ti-6Al-4V."}'
                '], "count": 5, "query": "laser powder bed fusion Ti-6Al-4V microstructure porosity defect"}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Good set of papers covering microstructure, defects (porosity, keyhole), residual stress, "
                "and process parameter optimization for LPBF Ti-6Al-4V. Let me organize by topic.\n"
                "</think>\n\n"
                "Here are the key papers on LPBF of Ti-6Al-4V, organized by research focus:\n\n"
                "**Microstructure Formation**\n"
                "- Murr et al. (2012, *JMRT*, 1890 citations): Columnar prior-β grains → martensitic α' in "
                "as-built state; HIP+anneal restores equiaxed α+β and ductility. Essential baseline reference.\n"
                "- DebRoy et al. (2021, *Prog. Mater. Sci.*, 3200 citations): Comprehensive review — rotating "
                "scan strategies promote columnar-to-equiaxed transition and reduce texture anisotropy.\n\n"
                "**Defect Formation**\n"
                "- Zhao et al. (2017, *Scientific Reports*, 2140 citations): In-situ synchrotron X-ray imaging "
                "directly observing keyhole and lack-of-fusion pore nucleation in real time.\n"
                "- Greitemeier et al. (2017, *Int. J. Fatigue*, 870 citations): Volumetric energy density "
                "55–65 J/mm³ achieves <0.1% porosity; surface roughness is the dominant fatigue initiation site.\n\n"
                "**Residual Stress**\n"
                "- Shiomi et al. (2004, *CIRP Annals*, 1230 citations): Neutron diffraction shows tensile "
                "residual stresses up to 400 MPa; 650°C/3h stress relief reduces this by ~70%.\n\n"
                "**Practical takeaways for your research:**\n"
                "1. As-built LPBF Ti-6Al-4V always shows α' martensite — post-process heat treatment is "
                "required for aerospace fatigue certification.\n"
                "2. Keyhole porosity (deep melt pool instability) and lack-of-fusion porosity (insufficient "
                "energy) have different signatures and require different process adjustments.\n"
                "3. Scan strategy is a viable lever to control grain morphology without composition changes.\n\n"
                "Want me to search for more recent work (2023–2026) on in-situ monitoring or machine learning "
                "for LPBF process optimization?"
            ),
        },
    ]
}

_CONV_06 = {
    "messages": [
        {
            "role": "system",
            "content": (
                "You are PRISM, a materials discovery AI for nuclear and fusion energy applications. "
                "You assist with identifying radiation-resistant structural materials. "
                "Use tools to search databases and knowledge graphs. Think before calling tools."
            ),
        },
        {
            "role": "user",
            "content": "What tungsten alloys are known to be radiation-resistant? I need candidates for fusion reactor first-wall and divertor applications.",
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Tungsten (W) is the primary plasma-facing material candidate for fusion reactors (ITER divertor "
                "is W, EU-DEMO first wall candidates include W). Key radiation damage concerns for W:\n"
                "- High displacement damage (dpa) from 14 MeV neutrons\n"
                "- Transmutation: W→Re→Os under neutron irradiation\n"
                "- Void swelling, radiation hardening, embrittlement\n"
                "Radiation-resistant W alloys:\n"
                "- W-Re (5-26 wt% Re): reduces DBTT, better ductility but Re becomes radioactive, expensive\n"
                "- W-Ta: Ta has lower activation, similar BCC structure\n"
                "- W-V: V is low-activation\n"
                "- W-TiC, W-Y2O3, W-La2O3: ODS (oxide-dispersion strengthened) W\n"
                "- W-Cr (nanostructured): for RAFM-compatible joining\n"
                "I should do a search_materials for W-containing alloys and also a semantic_search for "
                "radiation resistance context.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "search_materials",
                        "arguments": {
                            "elements": ["W", "Re"],
                            "limit": 10,
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "search_materials",
            "tool_call_id": "call_1",
            "content": (
                '{"results": ['
                '{"id": "mp-91", "formula": "W", "elements": ["W"], '
                '"space_group": "Im-3m", "formation_energy": {"value": 0.0, "unit": "eV/atom"}, '
                '"sources": ["mp", "optimade"]},'
                '{"id": "mp-11734", "formula": "Re", "elements": ["Re"], '
                '"space_group": "P63/mmc", "sources": ["mp"]},'
                '{"id": "mp-570747", "formula": "W3Re", "elements": ["Re", "W"], '
                '"space_group": "Pm-3n", "formation_energy": {"value": -0.08, "unit": "eV/atom"}, '
                '"sources": ["mp"]},'
                '{"id": "mp-2814", "formula": "WRe", "elements": ["Re", "W"], '
                '"space_group": "Cmcm", "formation_energy": {"value": -0.15, "unit": "eV/atom"}, '
                '"sources": ["mp"]},'
                '{"id": "mp-1007543", "formula": "W2Re", "elements": ["Re", "W"], '
                '"space_group": "I4/mmm", "formation_energy": {"value": -0.11, "unit": "eV/atom"}, '
                '"sources": ["mp"]}'
                '], "count": 5, "search_time_ms": 198, "cached": false, "warnings": [], '
                '"providers_queried": [{"id": "mp", "status": "ok", "results": 5}]}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Got W-Re intermetallic phases from Materials Project. Now I should do a semantic search "
                "to get context on radiation resistance properties and ODS tungsten.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_2",
                    "type": "function",
                    "function": {
                        "name": "semantic_search",
                        "arguments": {
                            "query": "tungsten alloy radiation resistance fusion reactor divertor neutron damage displacement",
                            "limit": 8,
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "semantic_search",
            "tool_call_id": "call_2",
            "content": (
                '{"results": ['
                '{"score": 0.96, "id": "doc_9921", "title": "W-Re alloys for fusion: radiation effects", '
                '"snippet": "W-5Re reduces DBTT from ~400°C to ~200°C; however neutron irradiation causes '
                'Re transmutation to Os, forming brittle sigma and chi phases above 600°C.", '
                '"source": "marc27_pgvector"},'
                '{"score": 0.93, "id": "doc_8832", "title": "ODS tungsten: W-Y2O3 under ion irradiation", '
                '"snippet": "W-1wt%Y2O3 ODS alloy shows 40% reduction in void swelling vs pure W under '
                'Fe ion irradiation at 500°C due to oxide particle sink strength.", '
                '"source": "marc27_pgvector"},'
                '{"score": 0.91, "id": "doc_7743", "title": "W-Ta binary: low-activation alternative to W-Re", '
                '"snippet": "W-10Ta retains BCC structure and shows improved recrystallization temperature '
                'vs W-Re with lower long-term activation, favored for DEMO applications.", '
                '"source": "marc27_pgvector"},'
                '{"score": 0.88, "id": "doc_6654", "title": "Nanostructured W for radiation tolerance", '
                '"snippet": "Grain boundaries in nanocrystalline W (grain size <100 nm) act as efficient '
                'point defect sinks, reducing radiation hardening and swelling by 50% vs coarse-grained W.", '
                '"source": "marc27_pgvector"}'
                '], "count": 4, "query": "tungsten alloy radiation resistance fusion reactor", '
                '"source": "marc27_pgvector"}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "Now I have a comprehensive picture. Let me compile the full answer about W alloys for fusion.\n"
                "</think>\n\n"
                "Here is a summary of radiation-resistant tungsten alloys for fusion first-wall and divertor:\n\n"
                "**1. W-Re Alloys (most studied)**\n"
                "- W-5Re and W-26Re: reduce DBTT from ~400°C → ~200°C; historically the ITER standard.\n"
                "- *Critical problem:* neutron irradiation transmutes Re → Os, forming brittle σ and χ "
                "intermetallic phases above 600°C (embrittlement worse than pure W at high fluence).\n"
                "- **Verdict:** Not suitable for long-term DEMO operation due to transmutation.\n\n"
                "**2. W-Ta (Low-Activation Alternative)**\n"
                "- W-10Ta: BCC structure retained, higher recrystallization temperature than W-Re, "
                "  lower long-term activation — preferred for DEMO by EU Fusion.\n\n"
                "**3. ODS Tungsten (W-Y₂O₃, W-La₂O₃, W-TiC)**\n"
                "- W-1wt%Y₂O₃ shows ~40% reduction in void swelling under ion irradiation.\n"
                "- Oxide particles act as point-defect sinks (trap vacancies and interstitials).\n"
                "- Manufacturing challenge: requires powder metallurgy + hot isostatic pressing.\n\n"
                "**4. Nanocrystalline W**\n"
                "- Grain size <100 nm introduces high grain boundary sink density.\n"
                "- 50% reduction in radiation hardening vs coarse-grained W; thermally unstable above "
                "  ~900°C (grain growth).\n\n"
                "**DFT data from Materials Project:**\n"
                "- W₃Re (Pm-3n), WRe (Cmcm), W₂Re (I4/mmm) — all negative formation energies, "
                "confirming ordered intermetallic stability. The W-Re solid solution regime (< 25 at% Re "
                "BCC) is what reactors actually use — not these ordered phases.\n\n"
                "**Recommendation for your application:**\n"
                "For near-term ITER operation: pure W or W-La₂O₃ (ODS). For DEMO long-term: W-Ta or "
                "W-Y₂O₃ ODS. Avoid W-Re for neutron-rich environments beyond 5 dpa.\n\n"
                "Want me to search for recent MD simulations of primary damage cascades in W alloys, "
                "or find suppliers and processing routes?"
            ),
        },
    ]
}

_CONV_07 = {
    "messages": [
        {
            "role": "system",
            "content": (
                "You are PRISM, an AI assistant for aerospace materials engineering. You have CALPHAD "
                "thermodynamic calculation capabilities. You help engineers compute phase diagrams for "
                "alloy development. Reason carefully before making tool calls."
            ),
        },
        {
            "role": "user",
            "content": "I need to calculate the Ni-Al-Ti ternary phase diagram for nickel superalloy design. Temperature range 800 to 1400°C. Use the COST507 database.",
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "The user wants a ternary Ni-Al-Ti phase diagram using CALPHAD. Ni-Al-Ti is a critical "
                "system for nickel superalloys — Al and Ti partition into the γ' (L12 Ni3(Al,Ti)) "
                "strengthening phase. Key phases:\n"
                "- γ (FCC solid solution matrix)\n"
                "- γ' (L12 ordered Ni3Al/Ni3Ti)\n"
                "- β (NiAl B2 ordered)\n"
                "- η (Ni3Ti D024)\n"
                "- δ (Ni2Al)\n"
                "COST507 is a standard CALPHAD database for light alloys (primarily Al-based). For Ni "
                "superalloys the standard database is typically TCNI or PanNi. However, the user specified "
                "COST507 — I should use what they ask. Temperature range 800-1400°C = 1073-1673 K. "
                "The tool expects Kelvin for temperature_range. Step of 25 K is reasonable.\n"
                "Components need to include 'VA' (vacancies) for most CALPHAD databases plus the actual "
                "elements. I'll include it.\n"
                "</think>"
            ),
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "calculate_phase_diagram",
                        "arguments": {
                            "database_name": "COST507",
                            "components": ["Ni", "Al", "Ti", "VA"],
                            "phases": ["FCC_A1", "L12_PRIME", "BCC_B2", "NI3TI_D024", "NI2AL"],
                            "temperature_range": [1073, 1673, 25],
                            "pressure": 101325,
                        },
                    },
                }
            ],
        },
        {
            "role": "tool",
            "name": "calculate_phase_diagram",
            "tool_call_id": "call_1",
            "content": (
                '{"status": "success", "database": "COST507", '
                '"components": ["Ni", "Al", "Ti", "VA"], '
                '"phases_calculated": ["FCC_A1", "L12_PRIME", "BCC_B2", "NI3TI_D024"], '
                '"temperature_range_K": [1073, 1673, 25], '
                '"phase_boundaries": {'
                '"gamma_gamma_prime_solvus": {'
                '"description": "γ/γ+γ\' two-phase boundary", '
                '"Al_fraction_range": [0.10, 0.22], "Ti_fraction_range": [0.02, 0.10], '
                '"solvus_temperature_K": 1523'
                '},'
                '"gamma_prime_stability": {'
                '"description": "L12 γ\' phase stable below solvus", '
                '"max_Ti_substitution": 0.35, "note": "Ti replaces Al on Al sublattice"'
                '},'
                '"beta_field": {'
                '"description": "BCC β NiAl phase", "Al_fraction": [0.45, 0.55], '
                '"stable_range_K": [1073, 1673]'
                '}'
                '}, '
                '"output_file": "Ni-Al-Ti_phase_diagram.json", '
                '"warnings": ["NI2AL phase not in COST507 database — skipped"], '
                '"calculation_time_s": 18.4}'
            ),
        },
        {
            "role": "assistant",
            "content": (
                "<think>\n"
                "The calculation succeeded. COST507 does not include NI2AL but handled the other phases. "
                "Let me interpret the results for the user in the context of superalloy design.\n"
                "</think>\n\n"
                "The Ni-Al-Ti ternary phase diagram calculation completed successfully (18.4 s). "
                "Here are the key results:\n\n"
                "**Phase boundaries identified (1073–1673 K / 800–1400°C):**\n\n"
                "| Phase Region | Al content | Ti content | Notes |\n"
                "|-------------|------------|------------|-------|\n"
                "| γ + γ' two-phase | 10–22 at% Al | 2–10 at% Ti | Target zone for superalloy design |\n"
                "| γ' solvus | — | — | ~1523 K (1250°C) — Al,Ti-dependent |\n"
                "| β (BCC NiAl) | 45–55 at% Al | — | Brittle intermetallic; avoid |\n\n"
                "**Key findings for superalloy design:**\n"
                "1. **γ' solvus at ~1250°C** — this sets your solution heat treatment temperature. "
                "   Solution treat above this, then age below for γ' precipitation.\n"
                "2. **Ti substitutes for Al in γ'** (up to 35 at% Ti on Al sublattice) — Ti increases γ' "
                "   volume fraction and lattice misfit without entering γ to the same degree, "
                "   improving high-temperature strength.\n"
                "3. **β phase field** forms at >45 at% Al — keep your Al content well below this.\n\n"
                "**Note:** NI2AL (Ni₂Al) phase was not found in COST507 and was skipped. If you need "
                "this phase, consider importing the TCNI9 database which has complete Ni superalloy coverage.\n\n"
                "The diagram is saved as `Ni-Al-Ti_phase_diagram.json`. Want me to calculate a specific "
                "equilibrium condition at a target composition (e.g., Ni-6Al-4Ti at 1100°C) or generate "
                "an isopleth at fixed Ti content?"
            ),
        },
    ]
}
