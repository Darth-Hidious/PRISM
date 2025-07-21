from pydantic import BaseModel
from typing import List, Optional

class MaterialBase(BaseModel):
    formula: str
    elements: List[str]
    provider: str
    source_id: str
    band_gap: Optional[float] = None

class MaterialCreate(MaterialBase):
    pass

class Material(MaterialBase):
    id: int

    class Config:
        orm_mode = True 