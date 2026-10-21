#pragma once
// A map that keeps the order its keys were first added in. The converter
// keeps its indexes this way: which stock asset of a name it takes, and the
// order it goes through them, follow the order the stock files were read.
//
// Values live in a vector: a reference or pointer FindOrAdd or Find gives
// holds only until the next key is added.
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

namespace mapport
{
    template <class K, class V, class Hash = std::hash<K>>
    class Ordered
    {
    public:
        // The value of a key, added (with `v`) when it isn't there yet.
        V& FindOrAdd(const K& k, V v = V())
        {
            auto [it, added] = index.emplace(k, items.size());
            if (added) items.emplace_back(k, std::move(v));
            return items[it->second].second;
        }
        void Set(const K& k, V v) { FindOrAdd(k) = std::move(v); }
        V* Find(const K& k)
        {
            auto it = index.find(k);
            return it == index.end() ? nullptr : &items[it->second].second;
        }
        const V* Find(const K& k) const
        {
            auto it = index.find(k);
            return it == index.end() ? nullptr : &items[it->second].second;
        }
        bool Has(const K& k) const { return index.count(k) != 0; }
        size_t Size() const { return items.size(); }
        bool Empty() const { return items.empty(); }
        auto begin() { return items.begin(); }
        auto end() { return items.end(); }
        auto begin() const { return items.begin(); }
        auto end() const { return items.end(); }

    private:
        std::vector<std::pair<K, V>> items;
        std::unordered_map<K, size_t, Hash> index;
    };

    // An asset's place in an index: its struct and its name.
    struct AssetKey
    {
        std::string type, name;
        bool operator==(const AssetKey& b) const { return type == b.type && name == b.name; }
    };
    struct AssetKeyHash
    {
        size_t operator()(const AssetKey& k) const
        {
            return std::hash<std::string>{}(k.type) * 31 ^ std::hash<std::string>{}(k.name);
        }
    };
    template <class V>
    using AssetIndex = Ordered<AssetKey, V, AssetKeyHash>;
}
