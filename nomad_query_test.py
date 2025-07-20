"""
Test different query formats for NOMAD API based on error message
Error: "wrong format, use <quantity>[__<op>]__<value>"
"""
import asyncio
import httpx
import json

async def test_nomad_query_formats():
    """Test various query formats to find the correct syntax"""
    base_url = "https://nomad-lab.eu/prod/rae/api/v1"
    
    # Different query formats to test
    test_queries = [
        # Format 1: Using __ operators
        {
            "name": "elements__any format",
            "query": {
                "results.material.elements__any": "Si"
            }
        },
        {
            "name": "elements__contains format", 
            "query": {
                "results.material.elements__contains": "Si"
            }
        },
        {
            "name": "elements__in format",
            "query": {
                "results.material.elements__in": ["Si"]
            }
        },
        # Format 2: Direct field queries
        {
            "name": "direct elements format",
            "query": {
                "results.material.elements": "Si"
            }
        },
        # Format 3: Using filters structure
        {
            "name": "filters structure",
            "query": {
                "filters": {
                    "results.material.elements": "Si"
                }
            }
        },
        # Format 4: Simple query
        {
            "name": "simple material query",
            "query": {
                "material.elements": "Si"
            }
        }
    ]
    
    async with httpx.AsyncClient(timeout=30.0) as client:
        for test in test_queries:
            print(f"\n{'='*50}")
            print(f"Testing: {test['name']}")
            print(f"Query: {json.dumps(test['query'], indent=2)}")
            
            try:
                # POST to /entries/query endpoint
                response = await client.post(
                    f"{base_url}/entries/query",
                    json={
                        "query": test["query"],
                        "pagination": {"page_size": 1}
                    }
                )
                
                print(f"Status: {response.status_code}")
                
                if response.status_code == 200:
                    data = response.json()
                    print(f"Success! Found {data.get('pagination', {}).get('total', 0)} results")
                    if data.get('data'):
                        print(f"Sample entry ID: {data['data'][0].get('entry_id', 'N/A')}")
                    break  # Found working format
                else:
                    print(f"Error: {response.text}")
                    
            except Exception as e:
                print(f"Exception: {e}")
        
        # Also test the /entries endpoint directly
        print(f"\n{'='*50}")
        print("Testing direct /entries endpoint with parameters")
        try:
            response = await client.get(
                f"{base_url}/entries",
                params={
                    "results.material.elements": "Si",
                    "page_size": 1
                }
            )
            print(f"Status: {response.status_code}")
            if response.status_code == 200:
                data = response.json()
                print(f"Success! Found {data.get('pagination', {}).get('total', 0)} results")
            else:
                print(f"Error: {response.text}")
        except Exception as e:
            print(f"Exception: {e}")

if __name__ == "__main__":
    asyncio.run(test_nomad_query_formats())
