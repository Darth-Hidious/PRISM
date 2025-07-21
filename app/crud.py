from sqlalchemy.orm import Session
from .db import models
from . import schemas

def get_material(db: Session, material_id: int):
    return db.query(models.Material).filter(models.Material.id == material_id).first()

def get_material_by_source_id(db: Session, source_id: str):
    return db.query(models.Material).filter(models.Material.source_id == source_id).first()

def get_materials(db: Session, skip: int = 0, limit: int = 100):
    return db.query(models.Material).offset(skip).limit(limit).all()

def create_material(db: Session, material: schemas.MaterialCreate):
    db_material = models.Material(**material.model_dump())
    db.add(db_material)
    db.commit()
    db.refresh(db_material)
    return db_material 