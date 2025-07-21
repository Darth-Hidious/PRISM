from sqlalchemy import Column, Integer, String, Float
from .database import Base

class Material(Base):
    __tablename__ = "materials"

    id = Column(String, primary_key=True, index=True)
    formula = Column(String)
    elements = Column(String)
    provider = Column(String)
    source_id = Column(String)
    band_gap = Column(Float) 