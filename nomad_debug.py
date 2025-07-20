#!/usr/bin/env python3
"""
Simple test script to debug NOMAD API connection issues.
"""

import asyncio
import httpx
import json

async def test_nomad_connection():
    """Test NOMAD API connection with different approaches."""
    
    base_url = "https://nomad-lab.eu/prod/v1/api/v1"
    
    print("🔍 Testing NOMAD API Connection...")
    print(f"Base URL: {base_url}")
    print("-" * 50)
    
    async with httpx.AsyncClient(timeout=30.0) as client:
        
        # Test 1: Simple GET request to entries
        print("📡 Test 1: GET /entries with minimal parameters")
        try:
            response = await client.get(
                f"{base_url}/entries",
                params={"page_size": 1}
            )
            print(f"   Status: {response.status_code}")
            print(f"   Headers: {dict(response.headers)}")
            if response.status_code == 200:
                data = response.json()
                print(f"   Response keys: {list(data.keys())}")
                print(f"   Total entries: {data.get('pagination', {}).get('total', 'N/A')}")
                print("   ✅ SUCCESS")
            else:
                print(f"   Response: {response.text[:200]}")
                print("   ❌ FAILED")
        except Exception as e:
            print(f"   Error: {e}")
            print("   ❌ FAILED")
        
        print()
        
        # Test 2: POST request to entries/query  
        print("📡 Test 2: POST /entries/query with JSON body")
        try:
            query_data = {
                "page_size": 1,
                "page": 0
            }
            
            response = await client.post(
                f"{base_url}/entries/query",
                json=query_data,
                headers={"Content-Type": "application/json"}
            )
            print(f"   Status: {response.status_code}")
            if response.status_code == 200:
                data = response.json()
                print(f"   Response keys: {list(data.keys())}")
                print(f"   Total entries: {data.get('pagination', {}).get('total', 'N/A')}")
                print("   ✅ SUCCESS")
            else:
                print(f"   Response: {response.text[:200]}")
                print("   ❌ FAILED")
        except Exception as e:
            print(f"   Error: {e}")
            print("   ❌ FAILED")
        
        print()
        
        # Test 3: GET with material query
        print("📡 Test 3: GET /entries with material formula query")
        try:
            response = await client.get(
                f"{base_url}/entries",
                params={
                    "page_size": 1,
                    "q": "results.material.elements:Si"
                }
            )
            print(f"   Status: {response.status_code}")
            if response.status_code == 200:
                data = response.json()
                print(f"   Response keys: {list(data.keys())}")
                print(f"   Found entries: {len(data.get('data', []))}")
                print("   ✅ SUCCESS")
            else:
                print(f"   Response: {response.text[:200]}")
                print("   ❌ FAILED")
        except Exception as e:
            print(f"   Error: {e}")
            print("   ❌ FAILED")
    
    print()
    print("🎯 Test Complete!")
    print("If all tests failed, you may need to:")
    print("1. Check your internet connection")
    print("2. Verify the NOMAD API is accessible")
    print("3. Check if authentication is required for your region")

if __name__ == "__main__":
    asyncio.run(test_nomad_connection())
