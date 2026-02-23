from sqlalchemy import Column, Integer, String, Float
from .database import Base


class Material(Base):
    __tablename__ = "materials"

    id = Column(Integer, primary_key=True, index=True, autoincrement=True)
    source_id = Column(String, unique=True, index=True)
    formula = Column(String, index=True)
    elements = Column(String)  # Comma-separated element symbols
    provider = Column(String)
    band_gap = Column(Float, nullable=True)
    formation_energy = Column(Float, nullable=True)
    energy_above_hull = Column(Float, nullable=True)
