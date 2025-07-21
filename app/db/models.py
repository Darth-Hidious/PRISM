from sqlalchemy import Column, Integer, String, Float, JSON
from .database import Base

class Material(Base):
    __tablename__ = "materials"

    id = Column(Integer, primary_key=True, index=True, autoincrement=True)
    source_id = Column(String, unique=True, index=True)
    formula = Column(String, index=True)
    elements = Column(String)  # Storing as a comma-separated string
    provider = Column(String)
    band_gap = Column(Float) 